import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { JSDOM } from "jsdom";
import { createSpicetifyShim, runLegacyCode, type Shim } from "../src/plugins/legacy";
import type { PlayerAdapter, PlayerState } from "../src/plugins/adapters";

function fakePlayer() {
  let state: PlayerState = { playing: false, title: "A", artist: "X", album: "", artwork: null };
  let listener: ((s: PlayerState) => void) | null = null;
  const log: string[] = [];
  const player: PlayerAdapter = {
    getState: () => state,
    play: () => { log.push("play"); },
    pause: () => { log.push("pause"); },
    next: () => { log.push("next"); },
    previous: () => { log.push("previous"); },
    seek: () => undefined,
    subscribe: (l) => { listener = l; return () => { listener = null; }; },
  };
  return { player, log, set: (s: Partial<PlayerState>) => { state = { ...state, ...s }; listener?.(state); }, hasListener: () => listener !== null };
}

let shim: Shim | null = null;
beforeEach(() => { document.body.innerHTML = ""; localStorage.clear(); });
afterEach(() => { shim?.dispose(); shim = null; delete (window as any).Spicetify; });

function make(player?: PlayerAdapter, fetchImpl?: typeof fetch) {
  const notify = vi.fn();
  shim = createSpicetifyShim({ doc: document, player, ui: { notify }, version: "0.1.0", fetchImpl });
  return { api: shim.api as any, notify };
}

describe("Spicetify shim", () => {
  it("namespaces LocalStorage", () => {
    const { api } = make();
    api.LocalStorage.set("k", 5);
    expect(api.LocalStorage.get("k")).toBe("5");
    expect(localStorage.getItem("ghostify:legacy:k")).toBe("5");
    localStorage.setItem("spotify-own", "keep");
    api.LocalStorage.remove("k");
    expect(api.LocalStorage.get("k")).toBeNull();
    api.LocalStorage.set("a", "1");
    api.LocalStorage.clear();
    expect(localStorage.getItem("spotify-own")).toBe("keep");
    expect(api.LocalStorage.get("a")).toBeNull();
  });

  it("shows notifications", () => {
    const { api, notify } = make();
    api.showNotification("hi", true);
    expect(notify).toHaveBeenCalledWith("hi", true);
  });

  it("emits songchange and onplaypause events and unsubscribes on dispose", () => {
    const f = fakePlayer();
    const { api } = make(f.player);
    const song = vi.fn();
    const pp = vi.fn();
    api.Player.addEventListener("songchange", song);
    api.Player.addEventListener("onplaypause", pp);
    f.set({ playing: true });
    expect(pp).toHaveBeenCalledOnce();
    expect(song).not.toHaveBeenCalled();
    f.set({ title: "B" });
    expect(song).toHaveBeenCalledWith({ data: expect.objectContaining({ title: "B" }) });
    api.Player.removeEventListener("songchange", song);
    f.set({ title: "C" });
    expect(song).toHaveBeenCalledOnce();
    shim!.dispose();
    expect(f.hasListener()).toBe(false);
  });

  it("controls the player and reports its state", () => {
    const f = fakePlayer();
    const { api } = make(f.player);
    expect(api.Player.isPlaying()).toBe(false);
    api.Player.togglePlay();
    api.Player.next();
    api.Player.back();
    f.set({ playing: true });
    api.Player.togglePlay();
    expect(f.log).toEqual(["play", "next", "previous", "pause"]);
    expect(api.Player.data.title).toBe("A");
    expect(() => api.Player.addEventListener("x", 5)).toThrow(TypeError);
  });

  it("without a player, controls throw and state is null", () => {
    const { api } = make();
    expect(api.Player.data).toBeNull();
    expect(api.Player.isPlaying()).toBe(false);
    expect(() => api.Player.play()).toThrow("not available");
  });

  it("registers keyboard shortcuts with modifiers", () => {
    const { api } = make();
    const cb = vi.fn();
    api.Keyboard.registerShortcut("ctrl+shift+k", cb);
    api.Keyboard.registerShortcut({ key: "m", alt: true }, cb);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "K", ctrlKey: true, shiftKey: true }));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", ctrlKey: true }));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "m", altKey: true }));
    expect(cb).toHaveBeenCalledTimes(2);
    expect(() => api.Keyboard.registerShortcut("ctrl+", cb)).toThrow("key");
    shim!.dispose();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "m", altKey: true }));
    expect(cb).toHaveBeenCalledTimes(2);
  });

  it("creates topbar buttons that click, and removes them on dispose", () => {
    const { api } = make();
    const onClick = vi.fn();
    const button = new api.Topbar.Button("Do it", "icon", onClick);
    const element = document.querySelector<HTMLButtonElement>("[data-ghostify-topbar] button")!;
    expect(element.textContent).toBe("Do it");
    element.click();
    expect(onClick).toHaveBeenCalledWith(button);
    button.disabled = true;
    expect(element.disabled).toBe(true);
    shim!.dispose();
    expect(document.querySelector("[data-ghostify-topbar]")).toBeNull();
  });

  it("CosmosAsync only speaks https and parses JSON", async () => {
    const fetchImpl = vi.fn(async () => new Response('{"ok":true}'));
    const { api } = make(undefined, fetchImpl as never);
    expect(await api.CosmosAsync.get("https://api.example.com/x")).toEqual({ ok: true });
    await api.CosmosAsync.post("https://api.example.com/x", { a: 1 });
    expect((fetchImpl.mock.calls[1] as unknown as [string, RequestInit])[1]).toMatchObject({ method: "POST", credentials: "omit", body: '{"a":1}' });
    await expect(api.CosmosAsync.get("http://x.example/")).rejects.toThrow("https");
    const failing = make(undefined, vi.fn(async () => new Response("", { status: 500 })) as never);
    await expect(failing.api.CosmosAsync.get("https://a.example/")).rejects.toThrow("500");
  });

  it("does not provide APIs it cannot back, and says which it supports", () => {
    const { api } = make();
    for (const missing of ["Platform", "ContextMenu", "Menu", "React", "GraphQL", "URI"]) expect(api[missing]).toBeUndefined();
    expect(api.__ghostify.supported.length).toBeGreaterThan(3);
  });
});

describe("runLegacyCode", () => {
  // The shared test document does not execute scripts, so a scripting jsdom window is used.
  const page = () => new JSDOM("<!doctype html><html><head></head><body></body></html>", { runScripts: "dangerously" });

  it("runs extension code in the page", () => {
    const dom = page();
    runLegacyCode(dom.window.document, "me/ext", "window.__ran = (window.__ran || 0) + 1;");
    expect((dom.window as any).__ran).toBe(1);
    expect(dom.window.document.querySelector("script[data-ghostify-extension]")).toBeNull();
  });

  it("extensions see the Spicetify global that the manager installs", () => {
    const dom = page();
    const local = createSpicetifyShim({ doc: dom.window.document, player: undefined, ui: { notify: vi.fn() }, version: "9.9.9" });
    (dom.window as any).Spicetify = local.api;
    runLegacyCode(dom.window.document, "me/ext", "window.__v = Spicetify.Config.version;");
    expect((dom.window as any).__v).toBe("9.9.9");
    local.dispose();
  });

  it("reports errors thrown while loading", () => {
    const dom = page();
    expect(() => runLegacyCode(dom.window.document, "me/ext", "throw new Error('bad ext')")).toThrow(/me\/ext.*failed to load.*bad ext/);
  });
});
