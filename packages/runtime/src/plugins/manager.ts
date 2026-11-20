/** Starts, stops and supervises the enabled plugins. */
import type { BridgeClient } from "../rpc";
import { type Adapters, createCapabilityHandler, type CapabilityHandler } from "./capabilities";
import { createSpicetifyShim, runLegacyCode, type Shim } from "./legacy";
import { SandboxHost, type FailureKind, type TransportFactory } from "./sandbox";
import type { PlayerAdapter } from "./adapters";

export interface EnabledPlugin {
  id: string;
  name: string;
  version: string;
  runtime: "sandboxed" | "legacy";
  permissions: string[];
  hosts: string[];
}

interface LoadedPlugin extends EnabledPlugin {
  sha256: string;
  code: string;
}

interface CrashOutcome {
  crashes_in_window: number;
  disabled: boolean;
  rolled_back_to: string | null;
}

export interface ManagerOptions {
  client: BridgeClient;
  doc: Document;
  adapters: Adapters;
  hookTimeoutMs?: number;
  transportFactory?: TransportFactory;
  fetchImpl?: typeof fetch;
  version: string;
  /** Local safety net if the bridge cannot be reached to apply the crash policy. */
  localCrashLimit?: number;
  localCrashWindowMs?: number;
  minRestartMs?: number;
  maxRestartMs?: number;
  now?: () => number;
}

interface Running {
  signature: string;
  stop: () => void;
  crashes: number[];
  restartTimer: ReturnType<typeof setTimeout> | null;
}

export class PluginManager {
  private readonly running = new Map<string, Running>();
  private chain: Promise<void> = Promise.resolve();
  private legacyShim: Shim | null = null;
  private readonly unsubscribe: Array<() => void> = [];
  private started = false;
  private readonly now: () => number;

  constructor(private readonly options: ManagerOptions) {
    this.now = options.now ?? Date.now;
  }

  get runningIds(): string[] {
    return [...this.running.keys()].sort();
  }

  /** Syncs now and again after every (re)connect and `plugins.changed` notification. */
  start(): void {
    if (this.started) return;
    this.started = true;
    const { client } = this.options;
    this.unsubscribe.push(
      client.onState((state) => {
        if (state === "open") void this.sync();
      }),
      client.on("plugins.changed", () => void this.sync()),
    );
    if (client.connectionState === "open") void this.sync();
  }

  stopAll(): void {
    this.started = false;
    for (const off of this.unsubscribe.splice(0)) off();
    for (const [id] of this.running) this.stopPlugin(id);
    this.legacyShim?.dispose();
    this.legacyShim = null;
  }

  /** Brings the running set in line with the database. Calls never overlap. */
  sync(): Promise<void> {
    const next = this.chain.then(() => this.doSync());
    this.chain = next.catch(() => undefined);
    return next;
  }

  private async doSync(): Promise<void> {
    const { client } = this.options;
    let wanted: EnabledPlugin[];
    try {
      wanted = await client.call<EnabledPlugin[]>("plugin.enabled");
    } catch (error) {
      console.warn("[ghostify] cannot read the plugin list", error);
      return;
    }
    const wantedIds = new Set(wanted.map((p) => p.id));
    for (const id of [...this.running.keys()]) if (!wantedIds.has(id)) this.stopPlugin(id);
    for (const plugin of wanted) {
      const signature = JSON.stringify([plugin.version, plugin.runtime, plugin.permissions, plugin.hosts]);
      const current = this.running.get(plugin.id);
      if (current?.signature === signature) continue;
      if (current) this.stopPlugin(plugin.id);
      await this.startPlugin(plugin, signature, current?.crashes ?? []);
    }
  }

  private stopPlugin(id: string): void {
    const entry = this.running.get(id);
    if (!entry) return;
    this.running.delete(id);
    if (entry.restartTimer !== null) clearTimeout(entry.restartTimer);
    entry.stop();
  }

  private async startPlugin(plugin: EnabledPlugin, signature: string, crashes: number[]): Promise<void> {
    const { client } = this.options;
    let loaded: LoadedPlugin;
    try {
      loaded = await client.call<LoadedPlugin>("plugin.load", { id: plugin.id });
    } catch (error) {
      console.warn(`[ghostify] cannot load plugin ${plugin.id}`, error);
      return;
    }
    const entry: Running = { signature, stop: () => undefined, crashes, restartTimer: null };
    this.running.set(plugin.id, entry);
    try {
      entry.stop = loaded.runtime === "legacy" ? this.startLegacy(loaded) : this.startSandboxed(loaded, entry);
    } catch (error) {
      this.running.delete(plugin.id);
      console.error(`[ghostify] plugin ${plugin.id} failed to start`, error);
      void this.reportCrash(plugin.id, "crash", String(error));
    }
  }

  private startSandboxed(plugin: LoadedPlugin, entry: Running): () => void {
    const { client, doc, adapters, fetchImpl } = this.options;
    const capabilities: CapabilityHandler = createCapabilityHandler({ client, plugin, adapters, doc, fetchImpl });
    const playerEvents = this.wirePlayer(plugin, adapters.player);
    const host = new SandboxHost({
      plugin,
      doc,
      transportFactory: this.options.transportFactory,
      timeoutMs: this.options.hookTimeoutMs ?? 5000,
      handle: (method, params) => capabilities.handle(method, params),
      onLog: (level, text) => console[level](`[plugin ${plugin.id}]`, text),
      onFailure: (kind, detail) => {
        capabilities.cleanup();
        playerEvents.off();
        void this.onFailure(plugin, entry, kind, detail);
      },
    });
    playerEvents.attach((state) => host.emit("player.change", state));
    host.start();
    void client.call("plugin.report_state", { id: plugin.id, state: "active" }).catch(() => undefined);
    return () => {
      host.stop();
      capabilities.cleanup();
      playerEvents.off();
    };
  }

  private wirePlayer(plugin: EnabledPlugin, player: PlayerAdapter | undefined) {
    let unsubscribe: (() => void) | null = null;
    return {
      attach: (send: (state: unknown) => void) => {
        if (player && plugin.permissions.includes("player.read")) unsubscribe = player.subscribe(send);
      },
      off: () => {
        unsubscribe?.();
        unsubscribe = null;
      },
    };
  }

  private startLegacy(plugin: LoadedPlugin): () => void {
    const { doc, adapters, version } = this.options;
    if (!this.legacyShim) {
      this.legacyShim = createSpicetifyShim({ doc, player: adapters.player, ui: adapters.ui, version, fetchImpl: this.options.fetchImpl });
      const win = doc.defaultView as (Window & { Spicetify?: unknown }) | null;
      if (win) win.Spicetify = this.legacyShim.api;
    }
    runLegacyCode(doc, plugin.id, plugin.code);
    // Page code cannot be taken back; stopping only prevents a restart on the next sync.
    return () => undefined;
  }

  private async onFailure(plugin: LoadedPlugin, entry: Running, kind: FailureKind, detail: string): Promise<void> {
    if (this.running.get(plugin.id) !== entry) return;
    console.error(`[ghostify] plugin ${plugin.id} ${kind === "timeout" ? "timed out" : "crashed"}: ${detail}`);
    const now = this.now();
    const window = this.options.localCrashWindowMs ?? 600_000;
    entry.crashes = [...entry.crashes.filter((t) => now - t < window), now];
    const outcome = await this.reportCrash(plugin.id, kind, detail);
    if (this.running.get(plugin.id) !== entry) return; // changed while reporting
    const localLimit = this.options.localCrashLimit ?? 3;
    if (outcome?.disabled || entry.crashes.length >= localLimit) {
      this.stopPlugin(plugin.id);
      if (!outcome && entry.crashes.length >= localLimit) console.error(`[ghostify] ${plugin.id} stopped after repeated crashes (the app could not be reached)`);
      return;
    }
    const base = this.options.minRestartMs ?? 1000;
    const delay = Math.min(this.options.maxRestartMs ?? 30_000, base * 2 ** (entry.crashes.length - 1));
    entry.restartTimer = setTimeout(() => {
      entry.restartTimer = null;
      if (this.running.get(plugin.id) !== entry) return;
      this.running.delete(plugin.id);
      void this.startPlugin({ ...plugin }, entry.signature, entry.crashes);
    }, delay);
  }

  private async reportCrash(id: string, kind: FailureKind, detail: string): Promise<CrashOutcome | null> {
    try {
      const outcome = await this.options.client.call<CrashOutcome>("plugin.report_crash", { id, kind, detail });
      if (outcome.disabled) void this.sync();
      return outcome;
    } catch (error) {
      console.warn("[ghostify] cannot report the crash", error);
      return null;
    }
  }
}
