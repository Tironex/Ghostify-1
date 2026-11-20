// Shapes of the JSON the Rust side returns (see crates/ghostify-app/src/api.rs).

export type DiskState =
  | { state: "missing" }
  | { state: "unpatched" }
  | { state: "patched"; ruleset_version: string; spotify_version: string | null }
  | { state: "unreadable"; reason: string };

export interface SpotifyInstall {
  id: number;
  kind: string;
  root_path: string;
  version: string | null;
  last_seen_at: number;
}

export interface StatusReport {
  install: SpotifyInstall;
  state: unknown;
  disk: DiskState;
  backups: number;
}

export type PatchOutcome =
  | "already_healthy"
  | "patched"
  | "repatched"
  | "upgraded"
  | "restored"
  | "nothing_to_restore";

export interface PatchReport {
  install_id: number;
  outcome: PatchOutcome;
}

export interface RepairResult {
  install_id: number;
  report?: PatchReport;
  error?: string;
}

export interface AppInfo {
  version: string;
  bridge_port: number;
  connected_loaders: number;
  data_dir: string;
  lyrics_dir: string;
}

export type ThemeMode = "system" | "dark" | "light";

export interface ThemeInfo {
  id: string;
  name: string;
  version: string;
  source: string;
  active: boolean;
  color_scheme: string;
  color_schemes: string[];
}

export type ScanVerdict = "clean" | "suspicious" | "blocked";

export interface ScanFinding {
  rule: string;
  severity: "info" | "warn" | "block";
  file: string;
  line: number;
  message: string;
}

export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  source: string;
  runtime: "sandboxed" | "legacy";
  enabled: boolean;
  state: string;
  requested: string[];
  granted: string[];
  scan: { verdict: ScanVerdict; findings: string[] } | null;
}

export interface PluginInstallResult {
  info: PluginInfo;
  report: { verdict: ScanVerdict; findings: ScanFinding[] };
  notes: string[];
}

export interface Listing {
  id: string;
  kind: "plugin" | "theme";
  name: string;
  author: string;
  repo: string;
  category: string;
  description: string;
  tags: string[];
  latest_version: string;
  stars: number;
  rating_avg: number | null;
  rating_count: number;
}

export interface CategoryCount {
  category: string;
  count: number;
}

export interface PackageInfo {
  listing_id: string;
  version: string;
  download_url: string;
  sha256: string;
  size: number;
  permissions: string[];
}

export interface UpdateInfo {
  listing_id: string;
  kind: "plugin" | "theme";
  current_version: string;
  new_version: string;
  new_permissions: string[];
}

export type RefreshOutcome = { result: "skipped" } | { result: "applied"; sequence: number; listings: number };

export type AutoUpdate = "off" | "safe_only" | "all";

export interface Config {
  general: { check_app_updates: boolean; [key: string]: unknown };
  patch: { auto_repatch: boolean; [key: string]: unknown };
  plugins: { auto_update: AutoUpdate; crash_threshold: number; [key: string]: unknown };
  marketplace: { index_url: string; refresh_interval_secs: number; [key: string]: unknown };
  extras: { discord_enabled: boolean; discord_client_id: string; lyrics_online: boolean };
  [key: string]: unknown;
}

export interface Track {
  title: string;
  artist: string;
  album: string;
  artwork: string | null;
  duration_ms: number | null;
}

export interface PlayerSnapshot {
  playing: boolean;
  track: Track | null;
  position_ms: number;
}

export type PlayerAction = "play" | "pause" | "toggle" | "next" | "previous";

export interface LyricLine {
  time_ms: number;
  text: string;
}

export interface Lyrics {
  synced: LyricLine[];
  plain: string | null;
  source: "local" | "online";
}

export interface LyricsCurrent {
  track: Track | null;
  lyrics: Lyrics | null;
}

export interface Hotkey {
  action: string;
  accelerator: string;
  enabled: boolean;
}
