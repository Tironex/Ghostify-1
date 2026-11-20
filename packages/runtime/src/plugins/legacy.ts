/**
 * Spicetify compatibility: extensions run directly in the Spotify page and see
 * a `Spicetify` global. They are NOT isolated and can do everything the page
 * can, so this runtime is off unless the user enables it, and crashes cannot
 * be detected reliably. Only the commonly used API is provided; unsupported
 * parts are simply absent so extensions that wait for them keep waiting
 * instead of failing in confusing ways.
 */
import type { PlayerAdapter, PlayerState, UiAdapter } from "./adapters";

export const SUPPORTED_API = [
  "Spicetify.LocalStorage",
  "Spicetify.showNotification",
  "Spicetify.Player (events songchange/onplaypause, data, isPlaying, play, pause, next, back, togglePlay)",
  "Spicetify.Keyboard.registerShortcut",
  "Spicetify.Topbar.Button",
  "Spicetify.Config",
  "Spicetify.CosmosAsync.get/post (https only)",
] as const;

type Listener = (event: { data: PlayerState | null }) => void;

export interface ShimOptions {
  doc: Document;
  player: PlayerAdapter | undefined;
  ui: UiAdapter;
  version: string;
  fetchImpl?: typeof fetch;
}

interface Shortcut {
  key: string;
  ctrl: boolean;
  shift: boolean;
  alt: boolean;
  meta: boolean;
  callback: (event: KeyboardEvent) => void;
}

const STORAGE_PREFIX = "ghostify:legacy:";

function parseKeys(keys: unknown): Omit<Shortcut, "callback"> {
  if (typeof keys === "string") {
    const parts = keys.split("+").map((p) => p.trim().toLowerCase());
    const key = parts.pop() ?? "";
    return { key, ctrl: parts.includes("ctrl") || parts.includes("control"), shift: parts.includes("shift"), alt: parts.includes("alt"), meta: parts.includes("meta") || parts.includes("cmd") };
  }
  const k = (keys ?? {}) as Record<string, unknown>;
  return { key: String(k.key ?? "").toLowerCase(), ctrl: Boolean(k.ctrl), shift: Boolean(k.shift), alt: Boolean(k.alt), meta: Boolean(k.meta) };
}

export interface Shim {
  api: Record<string, unknown>;
  /** Removes listeners, timers and buttons created by extensions. */
  dispose(): void;
}

export function createSpicetifyShim(options: ShimOptions): Shim {
  const { doc, player, ui } = options;
  const win = doc.defaultView;
  const listeners = new Map<string, Set<Listener>>();
  const shortcuts: Shortcut[] = [];
  const buttons: HTMLElement[] = [];
  let unsubscribe: (() => void) | null = null;
  let previous: PlayerState | null = null;

  const emit = (type: string, state: PlayerState | null) => {
    for (const listener of [...(listeners.get(type) ?? [])]) {
      try {
        listener({ data: state });
      } catch (error) {
        console.error(`[ghostify] Spicetify.Player listener for "${type}" failed`, error);
      }
    }
  };

  const ensureSubscription = () => {
    if (unsubscribe || !player) return;
    previous = player.getState();
    unsubscribe = player.subscribe((state) => {
      const old = previous;
      previous = state;
      if (!old || old.title !== state.title || old.artist !== state.artist) emit("songchange", state);
      if (!old || old.playing !== state.playing) emit("onplaypause", state);
    });
  };

  const requirePlayer = (): PlayerAdapter => {
    if (!player) throw new Error("the player is not available");
    return player;
  };

  const onKey = (event: KeyboardEvent) => {
    const key = event.key.toLowerCase();
    for (const s of shortcuts) {
      if (s.key === key && s.ctrl === event.ctrlKey && s.shift === event.shiftKey && s.alt === event.altKey && s.meta === event.metaKey) {
        event.preventDefault();
        s.callback(event);
      }
    }
  };
  win?.addEventListener("keydown", onKey);

  let bar: HTMLElement | null = null;
  const topbar = () => {
    if (!bar) {
      bar = doc.createElement("div");
      bar.setAttribute("data-ghostify-topbar", "");
      bar.style.cssText = "position:fixed;top:8px;right:140px;z-index:2147483646;display:flex;gap:6px";
      doc.body.appendChild(bar);
    }
    return bar;
  };

  const fetchJson = async (method: "GET" | "POST", url: string, body?: unknown): Promise<unknown> => {
    const target = new URL(url);
    if (target.protocol !== "https:") throw new Error("only https requests are supported");
    const response = await (options.fetchImpl ?? fetch)(target.toString(), {
      method,
      credentials: "omit",
      headers: body === undefined ? {} : { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!response.ok) throw new Error(`request failed with status ${response.status}`);
    return response.json();
  };

  const api: Record<string, unknown> = {
    LocalStorage: {
      get: (key: string) => win?.localStorage.getItem(STORAGE_PREFIX + key) ?? null,
      set: (key: string, value: string) => win?.localStorage.setItem(STORAGE_PREFIX + key, String(value)),
      remove: (key: string) => win?.localStorage.removeItem(STORAGE_PREFIX + key),
      clear: () => {
        const store = win?.localStorage;
        if (!store) return;
        for (const key of Object.keys(store)) if (key.startsWith(STORAGE_PREFIX)) store.removeItem(key);
      },
    },
    showNotification: (text: string, isError?: boolean) => ui.notify(String(text), Boolean(isError)),
    Config: { version: options.version, current_theme: "", color_scheme: "", extensions: [], custom_apps: [] },
    Player: {
      get data() {
        return player?.getState() ?? null;
      },
      origin: undefined,
      addEventListener(type: string, listener: Listener) {
        if (typeof listener !== "function") throw new TypeError("listener must be a function");
        let set = listeners.get(type);
        if (!set) listeners.set(type, (set = new Set()));
        set.add(listener);
        ensureSubscription();
      },
      removeEventListener(type: string, listener: Listener) {
        listeners.get(type)?.delete(listener);
      },
      isPlaying: () => player?.getState().playing ?? false,
      play: () => requirePlayer().play(),
      pause: () => requirePlayer().pause(),
      togglePlay: () => {
        const p = requirePlayer();
        if (p.getState().playing) p.pause();
        else p.play();
      },
      next: () => requirePlayer().next(),
      back: () => requirePlayer().previous(),
    },
    Keyboard: {
      registerShortcut(keys: unknown, callback: (event: KeyboardEvent) => void) {
        if (typeof callback !== "function") throw new TypeError("callback must be a function");
        const parsed = parseKeys(keys);
        if (!parsed.key) throw new TypeError("a shortcut needs a key");
        shortcuts.push({ ...parsed, callback });
      },
    },
    Topbar: {
      Button: class {
        element: HTMLButtonElement;
        constructor(
          public label: string,
          _icon: string,
          public onClick: (self: unknown) => void,
          disabled = false,
        ) {
          this.element = doc.createElement("button");
          this.element.textContent = String(label).slice(0, 40);
          this.element.title = String(label).slice(0, 80);
          this.element.disabled = disabled;
          this.element.style.cssText = "padding:4px 10px;border-radius:16px;border:0;background:#2a2a2a;color:#fff;cursor:pointer;font:12px sans-serif";
          this.element.addEventListener("click", () => this.onClick(this));
          topbar().appendChild(this.element);
          buttons.push(this.element);
        }
        set disabled(value: boolean) {
          this.element.disabled = value;
        }
        destroy() {
          this.element.remove();
        }
      },
    },
    CosmosAsync: {
      get: (url: string) => fetchJson("GET", url),
      post: (url: string, body?: unknown) => fetchJson("POST", url, body),
    },
    __ghostify: { supported: SUPPORTED_API },
  };

  return {
    api,
    dispose() {
      win?.removeEventListener("keydown", onKey);
      unsubscribe?.();
      unsubscribe = null;
      listeners.clear();
      shortcuts.length = 0;
      for (const button of buttons) button.remove();
      buttons.length = 0;
      bar?.remove();
      bar = null;
    },
  };
}

/** Runs extension code in the page. Throws if it fails while loading. */
export function runLegacyCode(doc: Document, id: string, code: string): void {
  const win = doc.defaultView;
  if (!win) throw new Error("no window available");
  let failure: string | null = null;
  const onError = (event: ErrorEvent) => {
    failure = event.message || "script error";
  };
  const onViolation = () => {
    failure = "the page's content security policy does not allow running extension code";
  };
  win.addEventListener("error", onError);
  doc.addEventListener("securitypolicyviolation", onViolation);
  try {
    const script = doc.createElement("script");
    script.setAttribute("data-ghostify-extension", id);
    script.textContent = `${code}\n//# sourceURL=ghostify-extension-${id.replace(/[^a-z0-9._-]/gi, "_")}.js`;
    doc.head.appendChild(script);
    script.remove();
  } finally {
    win.removeEventListener("error", onError);
    doc.removeEventListener("securitypolicyviolation", onViolation);
  }
  if (failure) throw new Error(`extension "${id}" failed to load: ${failure}`);
}
