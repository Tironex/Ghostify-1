import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { App } from "./App";
import { backend, renderWithToasts } from "./test-utils";

const base = {
  "app.info": () => ({ version: "0.1.0", bridge_port: 49421, connected_loaders: 0, data_dir: "/d", lyrics_dir: "/l" }),
  "patch.status": () => [],
  "theme.list": () => [],
  "theme.mode": () => "dark",
};

const playing = {
  playing: true,
  position_ms: 84000,
  track: { title: "Nightfall", artist: "58er_GHO$T", album: "A", artwork: null, duration_ms: 227000 },
};

describe("App shell", () => {
  it("shows the player bar with the current track and controls it", async () => {
    const calls = backend({ ...base, "player.state": () => playing, "player.command": () => ({}) });
    renderWithToasts(<App />);
    const bar = await screen.findByRole("contentinfo", { name: "Player" });
    expect(await screen.findByText("Nightfall")).toBeInTheDocument();
    expect(bar).toHaveTextContent("1:24");
    expect(bar).toHaveTextContent("3:47");
    expect(screen.getByRole("progressbar", { name: "Progress" })).toHaveAttribute("aria-valuenow", "37");
    await userEvent.click(screen.getByRole("button", { name: "Pause" }));
    expect(calls.of("player.command")).toEqual([{ action: "toggle" }]);
  });

  it("stays idle and disabled while Spotify is not connected", async () => {
    backend(base);
    renderWithToasts(<App />);
    expect(await screen.findByText("Nothing playing")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Play" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Next track" })).toBeDisabled();
  });

  it("opens pages from the dashboard shortcuts", async () => {
    backend({ ...base, "plugin.list": () => [] });
    renderWithToasts(<App />);
    await userEvent.click(await screen.findByRole("button", { name: "Manage plugins" }));
    expect(await screen.findByRole("heading", { name: "Plugins" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Plugins" })).toHaveAttribute("aria-current", "page");
  });
});
