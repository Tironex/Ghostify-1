import type { BridgeClient } from "./rpc";
import type { PlayerAdapter, UiAdapter } from "./plugins/adapters";

const ACTIONS = ["play", "pause", "toggle", "next", "previous"] as const;
type Action = (typeof ACTIONS)[number];

function isAction(value: unknown): value is Action {
  return typeof value === "string" && (ACTIONS as readonly string[]).includes(value);
}

/**
 * Connects the player to the Ghostify app: reports what is playing (mini
 * player, lyrics, Discord) and carries out commands (hotkeys, media keys).
 */
export class PlayerLink {
  private offState: (() => void) | null = null;
  private offCommand: (() => void) | null = null;
  private offPlayer: (() => void) | null = null;
  private timer: ReturnType<typeof setInterval> | null = null;

  constructor(
    private readonly client: BridgeClient,
    private readonly player: PlayerAdapter,
    private readonly ui: UiAdapter,
    private readonly resyncMs = 10_000,
  ) {}

  start(): void {
    if (this.offState) return;
    this.offState = this.client.onState((state) => (state === "open" ? this.connected() : this.disconnected()));
    this.offCommand = this.client.on("player.command", (params) => this.run((params as { action?: unknown } | null)?.action));
  }

  stop(): void {
    this.disconnected();
    this.offState?.();
    this.offCommand?.();
    this.offState = this.offCommand = null;
  }

  private connected(): void {
    this.disconnected();
    this.report();
    this.offPlayer = this.player.subscribe(() => this.report());
    // The position is not an event; a regular resync lets the app extrapolate it and notice seeking.
    this.timer = setInterval(() => this.report(), this.resyncMs);
  }

  private disconnected(): void {
    this.offPlayer?.();
    this.offPlayer = null;
    if (this.timer !== null) clearInterval(this.timer);
    this.timer = null;
  }

  private report(): void {
    const state = this.player.getState();
    const params = {
      playing: state.playing,
      title: state.title,
      artist: state.artist,
      album: state.album,
      artwork: state.artwork,
      position_ms: state.positionMs ?? null,
      duration_ms: state.durationMs ?? null,
    };
    // A dropped connection is retried by the client; the next report catches up.
    this.client.call("player.report", params).catch(() => undefined);
  }

  private run(action: unknown): void {
    if (!isAction(action)) return;
    try {
      switch (action) {
        case "play":
          this.player.play();
          break;
        case "pause":
          this.player.pause();
          break;
        case "toggle":
          if (this.player.getState().playing) this.player.pause();
          else this.player.play();
          break;
        case "next":
          this.player.next();
          break;
        case "previous":
          this.player.previous();
          break;
      }
      // Report soon so the app does not wait for the next poll.
      setTimeout(() => this.report(), 300);
    } catch (error) {
      this.ui.notify(`Ghostify: ${error instanceof Error ? error.message : String(error)}`, true);
    }
  }
}
