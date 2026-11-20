/**
 * What a sandboxed plugin may do. Every call is checked twice: against the
 * grants the host received, and against the database through the bridge, so a
 * revoked permission takes effect immediately.
 */
import type { BridgeClient } from "../rpc";
import { sanitizeCss } from "./css";
import type { PlayerAdapter, PlaylistAdapter, UiAdapter } from "./adapters";

export const REQUIRED_PERMISSION: Readonly<Record<string, string>> = {
  "storage.get": "storage",
  "storage.set": "storage",
  "storage.delete": "storage",
  "network.fetch": "network",
  "notify.show": "notifications",
  "ui.addStyle": "ui",
  "ui.removeStyle": "ui",
  "player.getState": "player.read",
  "player.play": "player.control",
  "player.pause": "player.control",
  "player.next": "player.control",
  "player.previous": "player.control",
  "player.seek": "player.control",
  "clipboard.write": "clipboard",
  "playlist.list": "playlist.read",
  "playlist.create": "playlist.write",
};

export const MAX_FETCH_BODY = 1024 * 1024;
export const MAX_FETCH_RESPONSE = 5 * 1024 * 1024;
export const FETCH_TIMEOUT_MS = 15_000;
const AUTH_CACHE_MS = 5_000;
const METHODS = new Set(["GET", "POST", "PUT", "PATCH", "DELETE"]);
const FORBIDDEN_HEADERS = new Set(["cookie", "host", "origin", "referer", "user-agent", "content-length"]);

export interface PluginInfo {
  id: string;
  permissions: readonly string[];
  hosts: readonly string[];
}

export interface Adapters {
  player?: PlayerAdapter;
  playlists?: PlaylistAdapter;
  ui: UiAdapter;
}

export interface CapabilityOptions {
  client: Pick<BridgeClient, "call">;
  plugin: PluginInfo;
  adapters: Adapters;
  doc: Document;
  fetchImpl?: typeof fetch;
  now?: () => number;
}

export class PermissionError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "PermissionError";
  }
}

export interface CapabilityHandler {
  handle(method: string, params?: unknown): Promise<unknown>;
  /** Removes everything the plugin added to the page. */
  cleanup(): void;
}

function record(params: unknown): Record<string, unknown> {
  return typeof params === "object" && params !== null ? (params as Record<string, unknown>) : {};
}

function str(value: unknown, label: string, max: number): string {
  if (typeof value !== "string" || value.length > max) throw new TypeError(`${label} must be a string of at most ${max} characters`);
  return value;
}

export function createCapabilityHandler(options: CapabilityOptions): CapabilityHandler {
  const { client, plugin, adapters, doc } = options;
  const granted = new Set(plugin.permissions);
  const now = options.now ?? Date.now;
  const authCache = new Map<string, number>();
  let styleElement: HTMLStyleElement | null = null;

  async function authorize(permission: string, host?: string): Promise<void> {
    if (!granted.has(permission)) throw new PermissionError(`this plugin does not hold the permission "${permission}"`);
    const key = `${permission}|${host ?? ""}`;
    const cached = authCache.get(key);
    if (cached !== undefined && now() - cached < AUTH_CACHE_MS) return;
    try {
      await client.call("plugin.authorize", { id: plugin.id, permission, host });
    } catch (error) {
      authCache.delete(key);
      throw new PermissionError(error instanceof Error ? error.message : "permission denied");
    }
    authCache.set(key, now());
  }

  async function networkFetch(params: Record<string, unknown>): Promise<unknown> {
    const url = new URL(str(params.url, "url", 2048));
    if (url.protocol !== "https:") throw new PermissionError("only https requests are allowed");
    if (url.username || url.password) throw new PermissionError("credentials in the url are not allowed");
    if (!plugin.hosts.some((h) => hostMatches(h, url.hostname))) throw new PermissionError(`the host "${url.hostname}" is not in the plugin manifest`);
    await authorize("network", url.hostname);

    const method = typeof params.method === "string" ? params.method.toUpperCase() : "GET";
    if (!METHODS.has(method)) throw new TypeError(`method ${method} is not allowed`);
    const headers: Record<string, string> = {};
    const given = record(params.headers);
    const names = Object.keys(given);
    if (names.length > 20) throw new TypeError("at most 20 headers are allowed");
    for (const name of names) {
      const value = given[name];
      if (typeof value !== "string" || value.length > 2048 || /[\r\n]/.test(name + value)) throw new TypeError("invalid header");
      if (FORBIDDEN_HEADERS.has(name.toLowerCase()) || name.toLowerCase().startsWith("sec-") || name.toLowerCase().startsWith("proxy-")) throw new PermissionError(`the header "${name}" is not allowed`);
      headers[name] = value;
    }
    let body: string | undefined;
    if (params.body !== undefined) {
      body = str(params.body, "body", MAX_FETCH_BODY);
      if (method === "GET") throw new TypeError("GET requests cannot have a body");
    }

    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
    try {
      const response = await (options.fetchImpl ?? fetch)(url.toString(), {
        method,
        headers,
        body,
        credentials: "omit",
        redirect: "manual",
        referrerPolicy: "no-referrer",
        cache: "no-store",
        signal: controller.signal,
      });
      if (response.type === "opaqueredirect" || (response.status >= 300 && response.status < 400)) {
        throw new PermissionError("redirects are not followed; request the final address directly");
      }
      const length = Number(response.headers.get("content-length") ?? "0");
      if (length > MAX_FETCH_RESPONSE) throw new Error("the response is too large");
      const buffer = await response.arrayBuffer();
      if (buffer.byteLength > MAX_FETCH_RESPONSE) throw new Error("the response is too large");
      return {
        status: response.status,
        ok: response.ok,
        headers: { "content-type": response.headers.get("content-type") ?? "" },
        body: new TextDecoder().decode(buffer),
      };
    } finally {
      clearTimeout(timer);
    }
  }

  async function handle(method: string, rawParams?: unknown): Promise<unknown> {
    const permission = REQUIRED_PERMISSION[method];
    if (!permission) throw new TypeError(`unknown method "${method}"`);
    const params = record(rawParams);
    if (method === "network.fetch") return networkFetch(params);
    await authorize(permission);

    switch (method) {
      case "storage.get":
        return client.call("plugin.storage.get", { id: plugin.id, key: str(params.key, "key", 128) });
      case "storage.set":
        return client.call("plugin.storage.set", { id: plugin.id, key: str(params.key, "key", 128), value: params.value ?? null });
      case "storage.delete":
        return client.call("plugin.storage.delete", { id: plugin.id, key: str(params.key, "key", 128) });
      case "notify.show":
        adapters.ui.notify(str(params.text, "text", 300));
        return null;
      case "ui.addStyle": {
        const css = sanitizeCss(str(params.css, "css", 200 * 1024));
        if (!styleElement) {
          styleElement = doc.createElement("style");
          styleElement.setAttribute("data-ghostify-plugin", plugin.id);
          doc.head.appendChild(styleElement);
        }
        styleElement.textContent = css;
        return null;
      }
      case "ui.removeStyle":
        styleElement?.remove();
        styleElement = null;
        return null;
      case "clipboard.write": {
        const text = str(params.text, "text", 100_000);
        const clipboard = doc.defaultView?.navigator.clipboard;
        if (!clipboard) throw new Error("the clipboard is not available");
        await clipboard.writeText(text);
        return null;
      }
      case "playlist.list":
        if (!adapters.playlists) throw new Error("playlists are not supported by this Spotify version");
        return adapters.playlists.list();
      case "playlist.create": {
        if (!adapters.playlists) throw new Error("playlists are not supported by this Spotify version");
        const uris = Array.isArray(params.uris) ? params.uris.map((u) => str(u, "uri", 200)) : [];
        if (uris.length > 500) throw new TypeError("at most 500 tracks per playlist");
        return adapters.playlists.create(str(params.name, "name", 100), uris);
      }
      default:
        return playerCall(method, params);
    }
  }

  function playerCall(method: string, params: Record<string, unknown>): unknown {
    const player = adapters.player;
    if (!player) throw new Error("the player is not available");
    switch (method) {
      case "player.getState":
        return player.getState();
      case "player.play": {
        player.play();
        return null;
      }
      case "player.pause": {
        player.pause();
        return null;
      }
      case "player.next": {
        player.next();
        return null;
      }
      case "player.previous": {
        player.previous();
        return null;
      }
      case "player.seek": {
        const position = params.positionMs;
        if (typeof position !== "number" || !Number.isFinite(position) || position < 0) throw new TypeError("positionMs must be a non-negative number");
        player.seek(position);
        return null;
      }
      default:
        throw new TypeError(`unknown method "${method}"`);
    }
  }

  return {
    handle,
    cleanup() {
      styleElement?.remove();
      styleElement = null;
    },
  };
}

export function hostMatches(allowed: string, host: string): boolean {
  const name = host.toLowerCase();
  if (allowed.startsWith("*.")) {
    const suffix = allowed.slice(1); // ".example.com"
    return name.length > suffix.length && name.endsWith(suffix);
  }
  return allowed === name;
}
