/** Access to the Spotify page. Everything here depends on Spotify's current UI. */

export interface PlayerState {
  playing: boolean;
  title: string;
  artist: string;
  album: string;
  artwork: string | null;
  /** Playback position and track length, when the page shows them. */
  positionMs?: number | null;
  durationMs?: number | null;
}

export interface PlayerAdapter {
  getState(): PlayerState;
  play(): void;
  pause(): void;
  next(): void;
  previous(): void;
  seek(positionMs: number): void;
  /** Calls `listener` whenever the state changes. Returns the unsubscribe function. */
  subscribe(listener: (state: PlayerState) => void): () => void;
}

export interface PlaylistAdapter {
  list(): Promise<unknown>;
  create(name: string, uris: string[]): Promise<unknown>;
}

export interface UiAdapter {
  notify(text: string, isError?: boolean): void;
}

const BUTTONS = {
  playPause: '[data-testid="control-button-playpause"]',
  next: '[data-testid="control-button-skip-forward"]',
  previous: '[data-testid="control-button-skip-back"]',
};

const CLOCKS = {
  position: '[data-testid="playback-position"]',
  duration: '[data-testid="playback-duration"]',
};

const EMPTY: PlayerState = { playing: false, title: "", artist: "", album: "", artwork: null, positionMs: null, durationMs: null };

/** `m:ss` or `h:mm:ss` to milliseconds; `null` for anything else (for example `-1:05`). */
export function parseClock(text: string | null | undefined): number | null {
  const match = /^(?:(\d{1,2}):)?(\d{1,3}):([0-5]\d)$/.exec((text ?? "").trim());
  if (!match) return null;
  const [, hours, minutes, seconds] = match;
  return ((Number(hours ?? 0) * 60 + Number(minutes)) * 60 + Number(seconds)) * 1000;
}

/** The part of the state that decides whether listeners are told; the position moves every second. */
function changeKey(state: PlayerState): string {
  return JSON.stringify({ ...state, positionMs: null });
}

/**
 * Reads the track from the browser's media session and presses the player's
 * buttons. Spotify may rename those buttons; then controls fail with a clear
 * error instead of silently doing nothing.
 */
export class MediaSessionPlayer implements PlayerAdapter {
  private readonly listeners = new Set<(state: PlayerState) => void>();
  private timer: ReturnType<typeof setInterval> | null = null;
  private last = "";

  constructor(
    private readonly doc: Document,
    private readonly pollMs = 1000,
  ) {}

  getState(): PlayerState {
    const session = (this.doc.defaultView?.navigator as Navigator | undefined)?.mediaSession;
    if (!session) return { ...EMPTY };
    const meta = session.metadata;
    const artwork = meta?.artwork?.length ? (meta.artwork[meta.artwork.length - 1]?.src ?? null) : null;
    return {
      playing: session.playbackState === "playing",
      title: meta?.title ?? "",
      artist: meta?.artist ?? "",
      album: meta?.album ?? "",
      artwork,
      positionMs: parseClock(this.doc.querySelector(CLOCKS.position)?.textContent),
      durationMs: parseClock(this.doc.querySelector(CLOCKS.duration)?.textContent),
    };
  }

  private press(selector: string): void {
    const button = this.doc.querySelector<HTMLElement>(selector);
    if (!button) throw new Error("the player control was not found; this Spotify version is not supported");
    button.click();
  }

  play(): void {
    if (!this.getState().playing) this.press(BUTTONS.playPause);
  }

  pause(): void {
    if (this.getState().playing) this.press(BUTTONS.playPause);
  }

  next(): void {
    this.press(BUTTONS.next);
  }

  previous(): void {
    this.press(BUTTONS.previous);
  }

  seek(): void {
    throw new Error("seeking is not supported by this Spotify version");
  }

  subscribe(listener: (state: PlayerState) => void): () => void {
    this.listeners.add(listener);
    if (this.timer === null) {
      this.last = changeKey(this.getState());
      this.timer = setInterval(() => this.poll(), this.pollMs);
    }
    return () => {
      this.listeners.delete(listener);
      if (this.listeners.size === 0 && this.timer !== null) {
        clearInterval(this.timer);
        this.timer = null;
      }
    };
  }

  private poll(): void {
    const state = this.getState();
    const serialised = changeKey(state);
    if (serialised === this.last) return;
    this.last = serialised;
    for (const listener of [...this.listeners]) listener(state);
  }
}

/** Small toast in the corner of the page. */
export class ToastUi implements UiAdapter {
  constructor(
    private readonly doc: Document,
    private readonly durationMs = 4000,
  ) {}

  notify(text: string, isError = false): void {
    const toast = this.doc.createElement("div");
    toast.setAttribute("data-ghostify-toast", "");
    toast.setAttribute("role", "status");
    toast.textContent = text.slice(0, 300);
    toast.style.cssText = `position:fixed;right:16px;bottom:96px;z-index:2147483647;max-width:320px;padding:10px 14px;border-radius:8px;font:14px sans-serif;color:#fff;background:${isError ? "#b3261e" : "#2a2a2a"};box-shadow:0 4px 16px rgba(0,0,0,.4)`;
    this.doc.body.appendChild(toast);
    setTimeout(() => toast.remove(), this.durationMs);
  }
}
