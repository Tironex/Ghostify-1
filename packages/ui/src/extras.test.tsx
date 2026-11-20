import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach } from "vitest";
import { App, isMiniPlayer } from "./App";
import type { Config, Hotkey, LyricsCurrent, PlayerSnapshot, Track } from "./api/types";
import { Extras } from "./pages/Extras";
import { activeLine, MiniPlayer } from "./pages/MiniPlayer";
import { backend, fail, renderWithToasts } from "./test-utils";

const track: Track = { title: "Song", artist: "Artist", album: "Album", artwork: "https://i.scdn.co/x", duration_ms: 200_000 };

beforeEach(() => vi.useFakeTimers({ shouldAdvanceTime: true }));
afterEach(() => {
  vi.useRealTimers();
  window.location.hash = "";
});

describe("activeLine", () => {
  it("finds the last line that has started", () => {
    const times = [5000, 10_000, 15_000];
    expect(activeLine(times, 0)).toBe(-1);
    expect(activeLine(times, 5000)).toBe(0);
    expect(activeLine(times, 14_999)).toBe(1);
    expect(activeLine(times, 99_999)).toBe(2);
    expect(activeLine([], 5)).toBe(-1);
  });
});

describe("MiniPlayer", () => {
  const lyrics: LyricsCurrent = {
    track,
    lyrics: { source: "local", plain: null, synced: [{ time_ms: 1000, text: "First line" }, { time_ms: 20_000, text: "Second line" }, { time_ms: 40_000, text: "" }] },
  };

  it("shows the track, progress and the line being sung", async () => {
    let position = 5000;
    backend({
      "player.state": () => ({ playing: true, track, position_ms: position }) satisfies PlayerSnapshot,
      "lyrics.current": () => lyrics,
    });
    renderWithToasts(<MiniPlayer />);
    expect(await screen.findByText("Song")).toBeInTheDocument();
    expect(screen.getByText("Artist")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Pause" })).toBeInTheDocument();
    expect(screen.getByText("0:05")).toBeInTheDocument();
    expect(screen.getByText("3:20")).toBeInTheDocument();
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "3");

    const first = await screen.findByText("First line");
    expect(first).toHaveAttribute("aria-current", "true");
    expect(screen.getByText("Second line")).not.toHaveAttribute("aria-current");

    position = 25_000;
    await waitFor(() => expect(screen.getByText("Second line")).toHaveAttribute("aria-current", "true"), { timeout: 3000 });
    expect(screen.getByText("First line")).not.toHaveAttribute("aria-current");
    expect(screen.getByText("♪")).toBeInTheDocument();
  });

  it("sends commands and refreshes", async () => {
    let playing = false;
    const rec = backend({
      "player.state": () => ({ playing, track, position_ms: 0 }),
      "lyrics.current": () => ({ track, lyrics: null }),
      "player.command": ({ action }) => { if (action === "toggle") playing = true; return null; },
    });
    renderWithToasts(<MiniPlayer />);
    await userEvent.click(await screen.findByRole("button", { name: "Play" }));
    expect(await screen.findByRole("button", { name: "Pause" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Next track" }));
    await userEvent.click(screen.getByRole("button", { name: "Previous track" }));
    expect(rec.of("player.command")).toEqual([{ action: "toggle" }, { action: "next" }, { action: "previous" }]);
  });

  it("explains missing lyrics and shows plain ones", async () => {
    let current: LyricsCurrent = { track, lyrics: null };
    backend({ "player.state": () => ({ playing: true, track, position_ms: 0 }), "lyrics.current": () => current });
    const first = renderWithToasts(<MiniPlayer />);
    expect(await screen.findByText(/No lyrics found/)).toHaveTextContent("Artist - Song.lrc");
    first.unmount();
    current = { track, lyrics: { source: "online", synced: [], plain: "Just words\nin lines" } };
    renderWithToasts(<MiniPlayer />);
    expect(await screen.findByText(/Just words/)).toBeInTheDocument();
  });

  it("handles silence and a lost connection", async () => {
    backend({ "player.state": () => ({ playing: false, track: null, position_ms: 0 }) });
    const view = renderWithToasts(<MiniPlayer />);
    expect(await screen.findByText(/Nothing is playing/)).toBeInTheDocument();
    view.unmount();
    backend({ "player.state": () => fail("internal", "internal error") });
    renderWithToasts(<MiniPlayer />);
    expect(await screen.findByRole("alert")).toHaveTextContent("internal error");
  });

  it("reports a failed command", async () => {
    backend({
      "player.state": () => ({ playing: true, track, position_ms: 0 }),
      "lyrics.current": () => ({ track, lyrics: null }),
      "player.command": () => fail("not_found", "Spotify is not connected"),
    });
    renderWithToasts(<MiniPlayer />);
    await userEvent.click(await screen.findByRole("button", { name: "Next track" }));
    expect(await screen.findByText("Spotify is not connected")).toBeInTheDocument();
  });
});

const hotkeys: Hotkey[] = [
  { action: "play_pause", accelerator: "CommandOrControl+Alt+Space", enabled: true },
  { action: "toggle_mini_player", accelerator: "CommandOrControl+Alt+M", enabled: false },
];
const config: Config = {
  general: { check_app_updates: true },
  patch: { auto_repatch: true },
  plugins: { auto_update: "safe_only", crash_threshold: 3 },
  marketplace: { index_url: "https://ghostify.github.io/registry/index.json", refresh_interval_secs: 21600 },
  extras: { discord_enabled: false, discord_client_id: "", lyrics_online: false },
};

describe("Extras", () => {
  const base = (extra: Record<string, (p: any) => unknown> = {}) =>
    backend({
      "hotkeys.list": () => hotkeys,
      "config.get": () => config,
      "app.info": () => ({ version: "0.1.0", bridge_port: 1, connected_loaders: 0, data_dir: "/d", lyrics_dir: "/home/me/.ghostify/lyrics" }),
      ...extra,
    });

  it("edits, resets and switches shortcuts", async () => {
    const rec = base({ "hotkeys.set": () => ({}), "hotkeys.reset": () => ({}) });
    renderWithToasts(<Extras />);
    const field = await screen.findByLabelText("Shortcut for Play / pause");
    const save = screen.getByRole("button", { name: "Save shortcut for Play / pause" });
    expect(save).toBeDisabled();
    await userEvent.clear(field);
    await userEvent.type(field, "ctrl+alt+p");
    await userEvent.click(save);
    await waitFor(() => expect(rec.of("hotkeys.set")).toEqual([{ action: "play_pause", accelerator: "ctrl+alt+p", enabled: true }]));

    await userEvent.click(screen.getByRole("switch", { name: "Enable shortcut for Show / hide mini player" }));
    await waitFor(() => expect(rec.of("hotkeys.set")[1]).toEqual({ action: "toggle_mini_player", accelerator: "CommandOrControl+Alt+M", enabled: true }));

    await userEvent.click(screen.getByRole("button", { name: "Reset shortcut for Play / pause" }));
    await waitFor(() => expect(rec.of("hotkeys.reset")).toEqual([{ action: "play_pause" }]));
  });

  it("shows why a shortcut was refused", async () => {
    base({ "hotkeys.set": () => fail("invalid_input", "invalid input: `A` needs a modifier such as Ctrl or Alt") });
    renderWithToasts(<Extras />);
    const field = await screen.findByLabelText("Shortcut for Play / pause");
    await userEvent.clear(field);
    await userEvent.type(field, "A");
    await userEvent.click(screen.getByRole("button", { name: "Save shortcut for Play / pause" }));
    expect(await screen.findByText(/needs a modifier/)).toBeInTheDocument();
  });

  it("saves Discord and lyrics settings without touching other settings", async () => {
    const rec = base({ "config.save": () => ({ restart_required: true }) });
    renderWithToasts(<Extras />);
    await userEvent.type(await screen.findByLabelText("Discord application ID"), " 123456789012345678 ");
    await userEvent.click(screen.getByRole("button", { name: "Save ID" }));
    await waitFor(() => expect(rec.of("config.save")).toHaveLength(1));
    expect(rec.of("config.save")[0].config.extras).toEqual({ discord_enabled: false, discord_client_id: "123456789012345678", lyrics_online: false });
    expect(rec.of("config.save")[0].config.plugins).toEqual(config.plugins);

    await userEvent.click(screen.getByRole("switch", { name: "Show my music in Discord" }));
    await userEvent.click(screen.getByRole("checkbox", { name: /Also look lyrics up/ }));
    await waitFor(() => expect(rec.of("config.save")).toHaveLength(3));
    expect(rec.of("config.save")[1].config.extras.discord_enabled).toBe(true);
    expect(rec.of("config.save")[2].config.extras.lyrics_online).toBe(true);
    expect(screen.getByText("/home/me/.ghostify/lyrics")).toBeInTheDocument();
  });

  it("says the mini player needs the desktop app when run elsewhere", async () => {
    base();
    renderWithToasts(<Extras />);
    await userEvent.click(await screen.findByRole("button", { name: "Mini player" }));
    expect(await screen.findByText(/needs the desktop app/)).toBeInTheDocument();
  });

  it("opens the mini player through the desktop shell", async () => {
    base();
    const invoke = vi.fn(async () => null);
    window.__TAURI__ = { core: { invoke } };
    try {
      renderWithToasts(<Extras />);
      await userEvent.click(await screen.findByRole("button", { name: "Mini player" }));
      expect(invoke).toHaveBeenCalledWith("toggle_mini_player", {});
    } finally {
      delete window.__TAURI__;
    }
  });
});

describe("mini player window", () => {
  it("is chosen by the address and has no navigation", async () => {
    expect(isMiniPlayer("#/mini")).toBe(true);
    expect(isMiniPlayer("")).toBe(false);
    window.location.hash = "#/mini";
    backend({ "player.state": () => ({ playing: false, track: null, position_ms: 0 }) });
    renderWithToasts(<App />);
    expect(await screen.findByText(/Nothing is playing/)).toBeInTheDocument();
    expect(screen.queryByRole("navigation")).not.toBeInTheDocument();
  });

  it("is reachable from the main navigation as Extras", async () => {
    backend({
      "app.info": () => ({ version: "0.1.0", bridge_port: 1, connected_loaders: 0, data_dir: "/d", lyrics_dir: "/l" }),
      "patch.status": () => [], "hotkeys.list": () => hotkeys, "config.get": () => config,
    });
    renderWithToasts(<App />);
    await userEvent.click(await screen.findByRole("button", { name: "Extras" }));
    expect(await screen.findByRole("heading", { name: "Extras" })).toBeInTheDocument();
    expect(within(screen.getByRole("navigation")).getByRole("button", { name: "Extras" })).toHaveAttribute("aria-current", "page");
  });
});
