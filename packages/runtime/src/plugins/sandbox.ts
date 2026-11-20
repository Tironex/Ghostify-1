/** Hosts one plugin in a sandboxed iframe and watches it. */
import { isGuestMessage, type GuestToHost, type HostToGuest } from "./protocol";

export interface Transport {
  send(message: HostToGuest): void;
  onMessage(listener: (message: unknown) => void): void;
  destroy(): void;
}

export type TransportFactory = (doc: Document) => Transport;

const GUEST_CSP = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'";

/** An iframe with an opaque origin and a CSP without network access. */
export const iframeTransport: TransportFactory = (doc) => {
  const win = doc.defaultView;
  if (!win) throw new Error("no window available for the sandbox");
  const guest = __GUEST_SOURCE__.replace(/<\/script/gi, "<\\/script");
  const iframe = doc.createElement("iframe");
  iframe.setAttribute("sandbox", "allow-scripts");
  iframe.setAttribute("aria-hidden", "true");
  iframe.setAttribute("referrerpolicy", "no-referrer");
  iframe.tabIndex = -1;
  iframe.style.cssText = "position:absolute;width:0;height:0;border:0;visibility:hidden;pointer-events:none";
  iframe.srcdoc = `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="${GUEST_CSP}"><script>${guest}</script></head><body></body></html>`;

  let listener: ((message: unknown) => void) | null = null;
  const onWindowMessage = (event: MessageEvent) => {
    // The sandbox has an opaque origin, so the sender is identified by its window.
    if (event.source !== iframe.contentWindow) return;
    listener?.(event.data);
  };
  win.addEventListener("message", onWindowMessage);
  doc.body.appendChild(iframe);

  return {
    send: (message) => iframe.contentWindow?.postMessage(message, "*"),
    onMessage: (l) => {
      listener = l;
    },
    destroy: () => {
      listener = null;
      win.removeEventListener("message", onWindowMessage);
      iframe.remove();
    },
  };
};

export type FailureKind = "crash" | "timeout";
export type SandboxState = "idle" | "starting" | "running" | "stopped" | "failed";

export interface SandboxOptions {
  plugin: { id: string; name: string; version: string; code: string };
  doc: Document;
  transportFactory?: TransportFactory;
  /** Handles a capability call. Rejections are returned to the plugin. */
  handle: (method: string, params: unknown) => Promise<unknown>;
  /** Heartbeat and start-up deadline. */
  timeoutMs: number;
  onFailure: (kind: FailureKind, detail: string) => void;
  onLog?: (level: "debug" | "info" | "warn" | "error", text: string) => void;
  maxCallsPerSecond?: number;
  now?: () => number;
}

export class SandboxHost {
  private transport: Transport | null = null;
  private currentState: SandboxState = "idle";
  private lastBeat = 0;
  private startTimer: ReturnType<typeof setTimeout> | null = null;
  private watchdog: ReturnType<typeof setInterval> | null = null;
  private windowStart = 0;
  private callsInWindow = 0;
  private readonly now: () => number;

  constructor(private readonly options: SandboxOptions) {
    this.now = options.now ?? Date.now;
  }

  get state(): SandboxState {
    return this.currentState;
  }

  start(): void {
    if (this.currentState !== "idle") throw new Error("a sandbox can only be started once");
    this.currentState = "starting";
    const factory = this.options.transportFactory ?? iframeTransport;
    try {
      this.transport = factory(this.options.doc);
    } catch (error) {
      this.fail("crash", `cannot create the sandbox: ${String(error)}`);
      return;
    }
    this.transport.onMessage((message) => this.receive(message));
    this.armStartTimer("the plugin did not start in time");
  }

  /** Delivers an event to the plugin if it is running. */
  emit(name: string, payload: unknown): void {
    if (this.currentState === "running") this.transport?.send({ t: "event", name, payload });
  }

  stop(): void {
    if (this.currentState === "stopped" || this.currentState === "failed") return;
    const wasRunning = this.currentState === "running";
    this.currentState = "stopped";
    if (wasRunning) this.transport?.send({ t: "stop" });
    this.teardown();
  }

  private armStartTimer(detail: string): void {
    this.clearStartTimer();
    this.startTimer = setTimeout(() => this.fail("timeout", detail), this.options.timeoutMs);
  }

  private clearStartTimer(): void {
    if (this.startTimer !== null) clearTimeout(this.startTimer);
    this.startTimer = null;
  }

  private teardown(): void {
    this.clearStartTimer();
    if (this.watchdog !== null) clearInterval(this.watchdog);
    this.watchdog = null;
    this.transport?.destroy();
    this.transport = null;
  }

  private fail(kind: FailureKind, detail: string): void {
    if (this.currentState === "failed" || this.currentState === "stopped") return;
    this.currentState = "failed";
    this.teardown();
    this.options.onFailure(kind, detail.slice(0, 500));
  }

  private receive(raw: unknown): void {
    if (this.currentState !== "starting" && this.currentState !== "running") return;
    if (!isGuestMessage(raw)) return;
    const message: GuestToHost = raw;
    switch (message.t) {
      case "ready":
        if (this.currentState === "starting") {
          const { id, name, version, code } = this.options.plugin;
          this.transport?.send({ t: "init", id, name, version, code });
          this.armStartTimer("the plugin did not finish starting in time");
        }
        break;
      case "started":
        if (this.currentState === "starting") {
          this.clearStartTimer();
          this.currentState = "running";
          this.lastBeat = this.now();
          this.watchdog = setInterval(() => this.checkBeat(), Math.max(100, Math.floor(this.options.timeoutMs / 4)));
        }
        break;
      case "hb":
        this.lastBeat = this.now();
        break;
      case "error":
        this.fail("crash", message.message);
        break;
      case "log":
        this.options.onLog?.(message.level, message.text);
        break;
      case "call":
        void this.handleCall(message.cid, message.method, message.params);
        break;
    }
  }

  private checkBeat(): void {
    if (this.currentState === "running" && this.now() - this.lastBeat > this.options.timeoutMs) {
      this.fail("timeout", "the plugin stopped responding");
    }
  }

  private async handleCall(cid: number, method: string, params: unknown): Promise<void> {
    const limit = this.options.maxCallsPerSecond ?? 200;
    const now = this.now();
    if (now - this.windowStart >= 1000) {
      this.windowStart = now;
      this.callsInWindow = 0;
    }
    if (++this.callsInWindow > limit) {
      this.transport?.send({ t: "result", cid, ok: false, error: "too many calls" });
      return;
    }
    try {
      const value = await this.options.handle(method, params);
      this.transport?.send({ t: "result", cid, ok: true, value: value ?? null });
    } catch (error) {
      const text = error instanceof Error ? error.message : String(error);
      this.transport?.send({ t: "result", cid, ok: false, error: text.slice(0, 500) });
    }
  }
}
