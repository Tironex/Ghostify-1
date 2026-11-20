/**
 * Runs inside the sandboxed iframe of one plugin. It has no network, no access
 * to the Spotify page and talks to the host only through postMessage. The
 * plugin sees one global, `ghostify`.
 */
import type { HostToGuest } from "./protocol";

type Handler = (payload: unknown) => void;

const hostWindow = window.parent;
const pending = new Map<number, { resolve: (value: unknown) => void; reject: (error: Error) => void }>();
const handlers = new Map<string, Set<Handler>>();
let nextCall = 1;
let started = false;

function post(message: object): void {
  hostWindow.postMessage(message, "*");
}

function call(method: string, params?: unknown): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const cid = nextCall++;
    pending.set(cid, { resolve, reject });
    post({ t: "call", cid, method, params });
  });
}

function text(value: unknown): string {
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value) ?? String(value);
  } catch {
    return String(value);
  }
}

function log(level: "debug" | "info" | "warn" | "error") {
  return (...args: unknown[]) => post({ t: "log", level, text: args.map(text).join(" ").slice(0, 2000) });
}

function on(name: string, handler: Handler): () => void {
  if (typeof handler !== "function") throw new TypeError("handler must be a function");
  let set = handlers.get(name);
  if (!set) handlers.set(name, (set = new Set()));
  set.add(handler);
  return () => void set.delete(handler);
}

const api = Object.freeze({
  storage: Object.freeze({
    get: (key: string) => call("storage.get", { key }),
    set: (key: string, value: unknown) => call("storage.set", { key, value }),
    delete: (key: string) => call("storage.delete", { key }),
  }),
  network: Object.freeze({
    fetch: (url: string, init?: { method?: string; headers?: Record<string, string>; body?: string }) =>
      call("network.fetch", { url, ...init }),
  }),
  notify: (message: string) => call("notify.show", { text: message }),
  ui: Object.freeze({
    addStyle: (css: string) => call("ui.addStyle", { css }),
    removeStyle: () => call("ui.removeStyle"),
  }),
  player: Object.freeze({
    getState: () => call("player.getState"),
    play: () => call("player.play"),
    pause: () => call("player.pause"),
    next: () => call("player.next"),
    previous: () => call("player.previous"),
    seek: (positionMs: number) => call("player.seek", { positionMs }),
  }),
  clipboard: Object.freeze({ write: (value: string) => call("clipboard.write", { text: value }) }),
  playlists: Object.freeze({
    list: () => call("playlist.list"),
    create: (name: string, uris: string[]) => call("playlist.create", { name, uris }),
  }),
  on,
  log: Object.freeze({ debug: log("debug"), info: log("info"), warn: log("warn"), error: log("error") }),
});

Object.defineProperty(window, "ghostify", { value: api, writable: false, configurable: false });

function dispatch(name: string, payload: unknown): void {
  for (const handler of [...(handlers.get(name) ?? [])]) {
    try {
      handler(payload);
    } catch (error) {
      post({ t: "error", message: `event handler "${name}" failed: ${String(error)}` });
    }
  }
}

function start(code: string): void {
  if (started) return;
  started = true;
  const script = document.createElement("script");
  script.textContent = code;
  document.head.appendChild(script);
  script.remove();
  post({ t: "started" });
}

window.addEventListener("message", (event: MessageEvent) => {
  if (event.source !== hostWindow) return;
  const message = event.data as HostToGuest | null;
  if (typeof message !== "object" || message === null) return;
  switch (message.t) {
    case "init":
      start(String(message.code));
      break;
    case "result": {
      const entry = pending.get(message.cid);
      if (!entry) return;
      pending.delete(message.cid);
      if (message.ok) entry.resolve(message.value);
      else entry.reject(new Error(message.error));
      break;
    }
    case "event":
      dispatch(String(message.name), message.payload);
      break;
    case "stop":
      dispatch("stop", null);
      break;
  }
});

window.addEventListener("error", (event) => post({ t: "error", message: event.message || "script error" }));
// A rejected promise (for example a denied permission) is the plugin's own business, not a crash.
window.addEventListener("unhandledrejection", (event) => {
  event.preventDefault();
  log("warn")("unhandled rejection:", event.reason);
});

setInterval(() => post({ t: "hb" }), 1000);
post({ t: "ready" });
