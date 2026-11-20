import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MediaSessionPlayer, parseClock, type PlayerAdapter, type PlayerState, type UiAdapter } from "../src/plugins/adapters";
import { PlayerLink } from "../src/player-link";
import { BridgeClient } from "../src/rpc";
import { FakeSocket, flush } from "./fake";

const SECRET = "k".repeat(64);

beforeEach(() => {
  vi.useFakeTimers();
  FakeSocket.reset();
});
afterEach(() => vi.useRealTimers());

function fakePlayer(initial: Partial<PlayerState> = {}) {
  let state: PlayerState = { playing: true, title: "Song", artist: "Artist", album: "Album", artwork: "https://i.scdn.co/x", positionMs: 5000, durationMs: 180000, ...initial };
  const listeners = new Set<(s: PlayerState) => void>();
  const calls: string[] = [];
  const player: PlayerAdapter = {
    getState: () => state,
    play: vi.fn(() => calls.push("play")),
    pause: vi.fn(() => calls.push("pause")),
    next: vi.fn(() => calls.push("next")),
    previous: vi.fn(() => calls.push("previous")),
    seek: vi.fn(),
    subscribe: (l) => (listeners.add(l), () => listeners.delete(l)),
  };
  return { player, calls, listeners, set: (s: Partial<PlayerState>) => ((state = { ...state, ...s }), listeners.forEach((l) => l(state))) };
}

function setup(initial: Partial<PlayerState> = {}) {
  const fake = fakePlayer(initial);
  const client = new BridgeClient({ urls: ["ws://127.0.0.1:1/"], secret: SECRET, socketFactory: (u) => new FakeSocket(u), random: () => 0 });
  const ui: UiAdapter = { notify: vi.fn() };
  const link = new PlayerLink(client, fake.player, ui, 10_000);
  link.start();
  client.start();
  return { fake, client, ui, link, socket: () => FakeSocket.last };
}

const reports = (socket: FakeSocket) => socket.sent.filter((m) => m.method === "player.report");

describe("parseClock", () => {
  it("reads m:ss and h:mm:ss", () => {
    expect(parseClock("0:05")).toBe(5000);
    expect(parseClock("3:07")).toBe(187_000);
    expect(parseClock("1:02:03")).toBe(3_723_000);
    expect(parseClock(" 12:00 ")).toBe(720_000);
  });
  it("refuses anything else", () => {
    for (const bad of ["", null, undefined, "-1:05", "1:5", "abc", "1:60", "1:2:3:4"]) expect(parseClock(bad)).toBeNull();
  });
});

describe("MediaSessionPlayer position", () => {
  it("reads position and duration from the page and does not treat them as changes", () => {
    document.body.innerHTML = '<span data-testid="playback-position">0:42</span><span data-testid="playback-duration">3:30</span>';
    Object.defineProperty(navigator, "mediaSession", {
      configurable: true,
      value: { playbackState: "playing", metadata: { title: "T", artist: "A", album: "B", artwork: [] } },
    });
    const player = new MediaSessionPlayer(document, 1000);
    expect(player.getState()).toMatchObject({ positionMs: 42_000, durationMs: 210_000, title: "T" });
    const seen: PlayerState[] = [];
    const off = player.subscribe((s) => seen.push(s));
    document.querySelector('[data-testid="playback-position"]')!.textContent = "0:43";
    vi.advanceTimersByTime(1000);
    expect(seen).toHaveLength(0);
    (navigator.mediaSession as any).metadata.title = "Other";
    vi.advanceTimersByTime(1000);
    expect(seen).toHaveLength(1);
    off();
    document.body.innerHTML = "";
  });
});

describe("PlayerLink", () => {
  it("reports after connecting, on changes and regularly", async () => {
    const { fake, socket } = setup();
    expect(socket().sent).toHaveLength(0);
    socket().authenticate();
    await flush();
    const first = reports(socket());
    expect(first).toHaveLength(1);
    expect(first[0]!.params).toEqual({ playing: true, title: "Song", artist: "Artist", album: "Album", artwork: "https://i.scdn.co/x", position_ms: 5000, duration_ms: 180000 });

    fake.set({ title: "Next song", positionMs: 0 });
    expect(reports(socket())).toHaveLength(2);
    expect(reports(socket())[1]!.params.title).toBe("Next song");

    vi.advanceTimersByTime(10_000);
    expect(reports(socket())).toHaveLength(3);
  });

  it("stops reporting when the connection drops and resumes after reconnecting", async () => {
    const { fake, socket } = setup();
    socket().authenticate();
    await flush();
    const first = socket();
    first.close();
    expect(fake.listeners.size).toBe(0);
    vi.advanceTimersByTime(30_000);
    expect(reports(first)).toHaveLength(1);
    vi.advanceTimersByTime(1000);
    const second = socket();
    expect(second).not.toBe(first);
    second.authenticate();
    await flush();
    expect(reports(second)).toHaveLength(1);
    expect(fake.listeners.size).toBe(1);
  });

  it("carries out commands from the app and reports the result soon after", async () => {
    const { fake, socket } = setup({ playing: false });
    socket().authenticate();
    await flush();
    socket().push("player.command", { action: "toggle" });
    socket().push("player.command", { action: "next" });
    socket().push("player.command", { action: "previous" });
    socket().push("player.command", { action: "pause" });
    expect(fake.calls).toEqual(["play", "next", "previous", "pause"]);
    const before = reports(socket()).length;
    vi.advanceTimersByTime(300);
    expect(reports(socket()).length).toBeGreaterThan(before);
  });

  it("toggles to pause while playing", async () => {
    const { fake, socket } = setup({ playing: true });
    socket().authenticate();
    await flush();
    socket().push("player.command", { action: "toggle" });
    expect(fake.calls).toEqual(["pause"]);
  });

  it("ignores unknown or malformed commands", async () => {
    const { fake, ui, socket } = setup();
    socket().authenticate();
    await flush();
    for (const params of [{ action: "explode" }, { action: 5 }, {}, null, "next"]) socket().push("player.command", params);
    expect(fake.calls).toEqual([]);
    expect(ui.notify).not.toHaveBeenCalled();
  });

  it("tells the user when the page has no matching control", async () => {
    const { fake, ui, socket } = setup();
    (fake.player.next as any).mockImplementation(() => {
      throw new Error("the player control was not found; this Spotify version is not supported");
    });
    socket().authenticate();
    await flush();
    socket().push("player.command", { action: "next" });
    expect(ui.notify).toHaveBeenCalledWith(expect.stringContaining("not supported"), true);
  });

  it("stop removes every listener and timer", async () => {
    const { fake, link, socket } = setup();
    socket().authenticate();
    await flush();
    link.stop();
    expect(fake.listeners.size).toBe(0);
    const count = reports(socket()).length;
    vi.advanceTimersByTime(60_000);
    socket().push("player.command", { action: "next" });
    expect(reports(socket()).length).toBe(count);
    expect(fake.calls).toEqual([]);
  });
});
