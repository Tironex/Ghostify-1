import { beforeEach, describe, expect, it, vi } from "vitest";
import { createCapabilityHandler, hostMatches, PermissionError, REQUIRED_PERMISSION } from "../src/plugins/capabilities";
import { sanitizeCss } from "../src/plugins/css";
import type { PlayerAdapter } from "../src/plugins/adapters";

function setup(overrides: { permissions?: string[]; hosts?: string[]; authorize?: (p: any) => unknown; fetchImpl?: typeof fetch; player?: PlayerAdapter } = {}) {
  const calls: Array<{ method: string; params: any }> = [];
  const client = {
    call: vi.fn(async (method: string, params?: unknown) => {
      calls.push({ method, params });
      if (method === "plugin.authorize" && overrides.authorize) return overrides.authorize(params);
      if (method === "plugin.authorize") return true;
      return `server:${method}`;
    }),
  };
  const notify = vi.fn();
  let now = 1000;
  const handler = createCapabilityHandler({
    client: client as never,
    plugin: { id: "me/p", permissions: overrides.permissions ?? Object.values(REQUIRED_PERMISSION), hosts: overrides.hosts ?? ["api.example.com", "*.cdn.org"] },
    adapters: { ui: { notify }, player: overrides.player },
    doc: document,
    fetchImpl: overrides.fetchImpl,
    now: () => now,
  });
  return { handler, client, calls, notify, advance: (ms: number) => (now += ms) };
}

function response(body: string, init: { status?: number; type?: ResponseType; headers?: Record<string, string> } = {}): Response {
  const r = new Response(body, { status: init.status ?? 200, headers: init.headers });
  if (init.type) Object.defineProperty(r, "type", { value: init.type });
  return r;
}

beforeEach(() => {
  document.head.innerHTML = "";
  document.body.innerHTML = "";
});

describe("permissions", () => {
  it("rejects methods the plugin has no grant for, without asking the server", async () => {
    const { handler, calls } = setup({ permissions: ["storage"] });
    await expect(handler.handle("notify.show", { text: "hi" })).rejects.toBeInstanceOf(PermissionError);
    await expect(handler.handle("player.play")).rejects.toThrow('"player.control"');
    expect(calls).toEqual([]);
  });

  it("rejects unknown methods", async () => {
    await expect(setup().handler.handle("fs.read", {})).rejects.toThrow("unknown method");
  });

  it("asks the server and honours its refusal (revoked permissions)", async () => {
    const { handler } = setup({
      authorize: () => {
        throw new Error("plugin me/p does not hold the permission");
      },
    });
    await expect(handler.handle("notify.show", { text: "x" })).rejects.toBeInstanceOf(PermissionError);
  });

  it("caches an authorisation briefly, then asks again", async () => {
    const { handler, calls, advance } = setup();
    await handler.handle("notify.show", { text: "a" });
    await handler.handle("notify.show", { text: "b" });
    expect(calls.filter((c) => c.method === "plugin.authorize")).toHaveLength(1);
    advance(6000);
    await handler.handle("notify.show", { text: "c" });
    expect(calls.filter((c) => c.method === "plugin.authorize")).toHaveLength(2);
  });

  it("every method in the table maps to a known permission", () => {
    const known = ["storage", "network", "notifications", "ui", "player.read", "player.control", "clipboard", "playlist.read", "playlist.write"];
    for (const permission of Object.values(REQUIRED_PERMISSION)) expect(known).toContain(permission);
  });
});

describe("storage and notifications", () => {
  it("forwards storage calls with the plugin id", async () => {
    const { handler, calls } = setup();
    expect(await handler.handle("storage.get", { key: "k" })).toBe("server:plugin.storage.get");
    await handler.handle("storage.set", { key: "k", value: { a: 1 } });
    await handler.handle("storage.delete", { key: "k" });
    expect(calls.map((c) => c.params)).toContainEqual({ id: "me/p", key: "k", value: { a: 1 } });
    await expect(handler.handle("storage.get", { key: 5 })).rejects.toThrow(TypeError);
    await expect(handler.handle("storage.get", { key: "x".repeat(129) })).rejects.toThrow(TypeError);
  });

  it("shows notifications through the adapter", async () => {
    const { handler, notify } = setup();
    await handler.handle("notify.show", { text: "hello" });
    expect(notify).toHaveBeenCalledWith("hello");
    await expect(handler.handle("notify.show", { text: "x".repeat(301) })).rejects.toThrow(TypeError);
  });
});

describe("network.fetch", () => {
  const ok = () => vi.fn(async () => response('{"a":1}', { headers: { "content-type": "application/json" } }));

  it("fetches an allowed https host with safe defaults", async () => {
    const fetchImpl = ok();
    const { handler, calls } = setup({ fetchImpl: fetchImpl as never });
    const result = (await handler.handle("network.fetch", { url: "https://api.example.com/v1?q=1" })) as any;
    expect(result).toMatchObject({ status: 200, ok: true, body: '{"a":1}', headers: { "content-type": "application/json" } });
    const [url, init] = fetchImpl.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("https://api.example.com/v1?q=1");
    expect(init).toMatchObject({ method: "GET", credentials: "omit", redirect: "manual", referrerPolicy: "no-referrer" });
    expect(calls.find((c) => c.method === "plugin.authorize")?.params).toEqual({ id: "me/p", permission: "network", host: "api.example.com" });
  });

  it("matches wildcard hosts exactly", async () => {
    const { handler } = setup({ fetchImpl: ok() as never });
    await handler.handle("network.fetch", { url: "https://a.b.cdn.org/x" });
    for (const url of ["https://cdn.org/x", "https://evilcdn.org/x", "https://api.example.com.evil.net/x", "https://example.com/"]) {
      await expect(handler.handle("network.fetch", { url })).rejects.toBeInstanceOf(PermissionError);
    }
    expect(hostMatches("*.cdn.org", "CDN.org")).toBe(false);
    expect(hostMatches("*.cdn.org", "x.CDN.org")).toBe(true);
  });

  it("refuses insecure, credentialed and malformed requests", async () => {
    const fetchImpl = ok();
    const { handler } = setup({ fetchImpl: fetchImpl as never });
    await expect(handler.handle("network.fetch", { url: "http://api.example.com/" })).rejects.toThrow("https");
    await expect(handler.handle("network.fetch", { url: "https://user:pw@api.example.com/" })).rejects.toThrow("credentials");
    await expect(handler.handle("network.fetch", { url: "not a url" })).rejects.toThrow();
    await expect(handler.handle("network.fetch", { url: "https://api.example.com/", method: "TRACE" })).rejects.toThrow("not allowed");
    await expect(handler.handle("network.fetch", { url: "https://api.example.com/", body: "x" })).rejects.toThrow("GET");
    await expect(handler.handle("network.fetch", { url: "https://api.example.com/", headers: { Cookie: "a=b" } })).rejects.toThrow("not allowed");
    await expect(handler.handle("network.fetch", { url: "https://api.example.com/", headers: { "Sec-Fetch-Mode": "x" } })).rejects.toThrow("not allowed");
    await expect(handler.handle("network.fetch", { url: "https://api.example.com/", headers: { A: "x\r\nB: y" } })).rejects.toThrow("invalid header");
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it("does not follow redirects", async () => {
    const { handler } = setup({ fetchImpl: vi.fn(async () => response("", { status: 302 })) as never });
    await expect(handler.handle("network.fetch", { url: "https://api.example.com/" })).rejects.toThrow("redirects");
    const opaque = setup({ fetchImpl: vi.fn(async () => response("", { type: "opaqueredirect" })) as never });
    await expect(opaque.handler.handle("network.fetch", { url: "https://api.example.com/" })).rejects.toThrow("redirects");
  });

  it("limits response size", async () => {
    const big = setup({ fetchImpl: vi.fn(async () => response("x".repeat(10), { headers: { "content-length": String(6 * 1024 * 1024) } })) as never });
    await expect(big.handler.handle("network.fetch", { url: "https://api.example.com/" })).rejects.toThrow("too large");
  });

  it("allows POST with a body", async () => {
    const fetchImpl = ok();
    const { handler } = setup({ fetchImpl: fetchImpl as never });
    await handler.handle("network.fetch", { url: "https://api.example.com/", method: "post", body: "{}", headers: { "Content-Type": "application/json" } });
    const init = (fetchImpl.mock.calls[0] as unknown as [string, RequestInit])[1];
    expect(init).toMatchObject({ method: "POST", body: "{}", headers: { "Content-Type": "application/json" } });
  });

  it("is refused when the network permission was not granted or the server says no", async () => {
    const fetchImpl = ok();
    const none = setup({ permissions: ["storage"], fetchImpl: fetchImpl as never });
    await expect(none.handler.handle("network.fetch", { url: "https://api.example.com/" })).rejects.toBeInstanceOf(PermissionError);
    const denied = setup({ fetchImpl: fetchImpl as never, authorize: () => { throw new Error("denied"); } });
    await expect(denied.handler.handle("network.fetch", { url: "https://api.example.com/" })).rejects.toBeInstanceOf(PermissionError);
    expect(fetchImpl).not.toHaveBeenCalled();
  });
});

describe("ui styles", () => {
  it("adds, replaces and removes one style element per plugin, sanitised", async () => {
    const { handler } = setup();
    await handler.handle("ui.addStyle", { css: "a{color:red} @import url(https://evil.example/x.css); b{background:url(https://evil.example/p.png)}" });
    const style = document.querySelector('style[data-ghostify-plugin="me/p"]')!;
    expect(style.textContent).not.toContain("evil");
    expect(style.textContent).toContain("a{color:red}");
    await handler.handle("ui.addStyle", { css: "c{}" });
    expect(document.querySelectorAll("style[data-ghostify-plugin]")).toHaveLength(1);
    expect(style.textContent).toBe("c{}");
    await handler.handle("ui.removeStyle");
    expect(document.querySelector("style[data-ghostify-plugin]")).toBeNull();
  });

  it("cleanup removes the style", async () => {
    const { handler } = setup();
    await handler.handle("ui.addStyle", { css: "a{}" });
    handler.cleanup();
    expect(document.querySelector("style[data-ghostify-plugin]")).toBeNull();
  });

  it("sanitizeCss keeps data urls and fragments, drops the rest", () => {
    expect(sanitizeCss("a{background:url(data:image/png;base64,AAA)}")).toContain("data:image/png");
    expect(sanitizeCss("a{fill:url(#grad)}")).toContain("url(#grad)");
    expect(sanitizeCss("a{b:url('/local.png')}")).toBe('a{b:url("")}');
    expect(sanitizeCss("a{width:expression(1);-moz-binding:x}")).not.toMatch(/expression|binding/);
    expect(() => sanitizeCss("x".repeat(200 * 1024 + 1))).toThrow();
  });
});

describe("player and playlists", () => {
  function fakePlayer(): PlayerAdapter & { log: string[] } {
    const log: string[] = [];
    return {
      log,
      getState: () => ({ playing: true, title: "T", artist: "A", album: "B", artwork: null }),
      play: () => void log.push("play"),
      pause: () => void log.push("pause"),
      next: () => void log.push("next"),
      previous: () => void log.push("previous"),
      seek: (ms) => void log.push(`seek ${ms}`),
      subscribe: () => () => undefined,
    };
  }

  it("reads and controls the player", async () => {
    const player = fakePlayer();
    const { handler } = setup({ player });
    expect(await handler.handle("player.getState")).toMatchObject({ title: "T", playing: true });
    for (const m of ["play", "pause", "next", "previous"]) expect(await handler.handle(`player.${m}`)).toBeNull();
    await handler.handle("player.seek", { positionMs: 1500 });
    expect(player.log).toEqual(["play", "pause", "next", "previous", "seek 1500"]);
    for (const bad of [-1, NaN, "5", undefined]) await expect(handler.handle("player.seek", { positionMs: bad })).rejects.toThrow(TypeError);
  });

  it("fails clearly without a player or playlist support", async () => {
    const { handler } = setup();
    await expect(handler.handle("player.getState")).rejects.toThrow("not available");
    await expect(handler.handle("playlist.list")).rejects.toThrow("not supported");
    await expect(handler.handle("playlist.create", { name: "x", uris: [] })).rejects.toThrow("not supported");
  });

  it("validates playlist creation", async () => {
    const create = vi.fn(async () => "id");
    const handler = createCapabilityHandler({
      client: { call: async () => true } as never,
      plugin: { id: "me/p", permissions: ["playlist.write"], hosts: [] },
      adapters: { ui: { notify: vi.fn() }, playlists: { list: async () => [], create } },
      doc: document,
    });
    expect(await handler.handle("playlist.create", { name: "Mix", uris: ["spotify:track:1"] })).toBe("id");
    expect(create).toHaveBeenCalledWith("Mix", ["spotify:track:1"]);
    await expect(handler.handle("playlist.create", { name: "Mix", uris: new Array(501).fill("u") })).rejects.toThrow("500");
    await expect(handler.handle("playlist.create", { name: 5 })).rejects.toThrow(TypeError);
  });
});

describe("clipboard", () => {
  it("writes text when granted", async () => {
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(window.navigator, "clipboard", { value: { writeText }, configurable: true });
    const { handler } = setup();
    await handler.handle("clipboard.write", { text: "copied" });
    expect(writeText).toHaveBeenCalledWith("copied");
    Object.defineProperty(window.navigator, "clipboard", { value: undefined, configurable: true });
    await expect(handler.handle("clipboard.write", { text: "x" })).rejects.toThrow("not available");
  });
});
