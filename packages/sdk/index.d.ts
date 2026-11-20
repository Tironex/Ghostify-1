/**
 * Types of the `ghostify` global that every sandboxed Ghostify plugin receives.
 * Each function needs the permission named in its comment, declared in
 * `plugin.json` and granted by the user. A call without the permission rejects
 * with an error; it does not crash the plugin.
 */

export interface PlayerState {
  playing: boolean;
  title: string;
  artist: string;
  album: string;
  artwork: string | null;
  /** Position in the track in milliseconds, if the page shows it. Not an event trigger: `player.change` does not fire for it. */
  positionMs?: number | null;
  durationMs?: number | null;
}

export interface FetchInit {
  method?: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
  headers?: Record<string, string>;
  /** Not allowed with GET. At most 1 MiB. */
  body?: string;
}

export interface FetchResult {
  status: number;
  ok: boolean;
  headers: { "content-type": string };
  /** Response body as text, at most 5 MiB. */
  body: string;
}

export interface Ghostify {
  storage: {
    /** permission: `storage`. Resolves to `null` for unknown keys. */
    get(key: string): Promise<unknown>;
    /** permission: `storage`. Values up to 256 KiB, up to 500 keys. */
    set(key: string, value: unknown): Promise<null>;
    delete(key: string): Promise<boolean>;
  };
  network: {
    /**
     * permission: `network`. Only https, only hosts listed in `plugin.json`,
     * no cookies, redirects are not followed.
     */
    fetch(url: string, init?: FetchInit): Promise<FetchResult>;
  };
  /** permission: `notifications`. */
  notify(text: string): Promise<null>;
  ui: {
    /** permission: `ui`. Replaces the plugin's stylesheet. Remote references are removed. */
    addStyle(css: string): Promise<null>;
    removeStyle(): Promise<null>;
  };
  player: {
    /** permission: `player.read`. */
    getState(): Promise<PlayerState>;
    /** permission: `player.control`. */
    play(): Promise<null>;
    pause(): Promise<null>;
    next(): Promise<null>;
    previous(): Promise<null>;
    seek(positionMs: number): Promise<null>;
  };
  /** permission: `clipboard`. */
  clipboard: { write(text: string): Promise<null> };
  playlists: {
    /** permission: `playlist.read`. */
    list(): Promise<unknown>;
    /** permission: `playlist.write`. At most 500 tracks. */
    create(name: string, uris: string[]): Promise<unknown>;
  };
  /**
   * Events: `player.change` (needs `player.read`, payload {@link PlayerState})
   * and `stop` (the plugin is being stopped). Returns the unsubscribe function.
   */
  on(event: "player.change", handler: (state: PlayerState) => void): () => void;
  on(event: "stop", handler: () => void): () => void;
  log: {
    debug(...args: unknown[]): void;
    info(...args: unknown[]): void;
    warn(...args: unknown[]): void;
    error(...args: unknown[]): void;
  };
}

declare global {
  const ghostify: Ghostify;
}
