-- Ghostify schema, version 1. Timestamps are Unix seconds.

CREATE TABLE settings (
    key        TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE spotify_installs (
    id           INTEGER PRIMARY KEY,
    kind         TEXT NOT NULL CHECK (kind IN (
                     'windows_desktop', 'windows_store', 'macos',
                     'linux_native', 'linux_flatpak', 'linux_snap', 'custom')),
    root_path    TEXT NOT NULL UNIQUE,
    version      TEXT,
    last_seen_at INTEGER NOT NULL
);

CREATE TABLE patch_state (
    install_id            INTEGER PRIMARY KEY REFERENCES spotify_installs(id) ON DELETE CASCADE,
    status                TEXT NOT NULL CHECK (status IN ('unpatched', 'patched', 'failed', 'restored')),
    applied_rules_version TEXT,
    spotify_version       TEXT,
    applied_at            INTEGER
);

CREATE TABLE patch_history (
    id         INTEGER PRIMARY KEY,
    install_id INTEGER NOT NULL REFERENCES spotify_installs(id) ON DELETE CASCADE,
    action     TEXT NOT NULL CHECK (action IN ('patch', 'restore', 'repair', 'repatch')),
    outcome    TEXT NOT NULL CHECK (outcome IN ('success', 'failure')),
    error      TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_patch_history_install ON patch_history(install_id, id DESC);

CREATE TABLE plugins (
    id           TEXT PRIMARY KEY NOT NULL,
    name         TEXT NOT NULL,
    version      TEXT NOT NULL,
    source       TEXT NOT NULL CHECK (source IN ('marketplace', 'github', 'local')),
    runtime      TEXT NOT NULL CHECK (runtime IN ('sandboxed', 'legacy')),
    enabled      INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    state        TEXT NOT NULL DEFAULT 'installed' CHECK (state IN (
                     'installed', 'loaded', 'active', 'suspended', 'crashed', 'unloaded', 'removed')),
    sha256       TEXT,
    installed_at INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE plugin_permissions (
    plugin_id  TEXT NOT NULL REFERENCES plugins(id) ON DELETE CASCADE,
    permission TEXT NOT NULL,
    granted_at INTEGER NOT NULL,
    PRIMARY KEY (plugin_id, permission)
) WITHOUT ROWID;

CREATE TABLE plugin_snapshots (
    id         INTEGER PRIMARY KEY,
    plugin_id  TEXT NOT NULL REFERENCES plugins(id) ON DELETE CASCADE,
    version    TEXT NOT NULL,
    path       TEXT NOT NULL,
    sha256     TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (plugin_id, version)
);

CREATE TABLE plugin_events (
    id         INTEGER PRIMARY KEY,
    plugin_id  TEXT NOT NULL REFERENCES plugins(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN (
                   'loaded', 'activated', 'deactivated', 'updated',
                   'crash', 'timeout', 'rollback', 'disabled')),
    detail     TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_plugin_events_plugin ON plugin_events(plugin_id, created_at);

CREATE TABLE themes (
    id           TEXT PRIMARY KEY NOT NULL,
    name         TEXT NOT NULL,
    version      TEXT NOT NULL,
    source       TEXT NOT NULL CHECK (source IN ('marketplace', 'github', 'local', 'import')),
    active       INTEGER NOT NULL DEFAULT 0 CHECK (active IN (0, 1)),
    color_scheme TEXT NOT NULL DEFAULT 'default',
    installed_at INTEGER NOT NULL
) WITHOUT ROWID;
CREATE UNIQUE INDEX idx_themes_single_active ON themes(active) WHERE active = 1;

CREATE TABLE market_listings (
    id             TEXT PRIMARY KEY NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('plugin', 'theme')),
    name           TEXT NOT NULL,
    author         TEXT NOT NULL,
    repo           TEXT NOT NULL,
    category       TEXT NOT NULL,
    description    TEXT NOT NULL DEFAULT '',
    tags           TEXT NOT NULL DEFAULT '[]',
    latest_version TEXT NOT NULL,
    stars          INTEGER NOT NULL DEFAULT 0,
    rating_avg     REAL,
    rating_count   INTEGER NOT NULL DEFAULT 0,
    index_version  INTEGER NOT NULL
);
CREATE INDEX idx_market_listings_browse ON market_listings(kind, category, stars DESC);

CREATE VIRTUAL TABLE market_fts USING fts5(
    name, description, tags,
    content = 'market_listings',
    content_rowid = 'rowid'
);

CREATE TRIGGER market_listings_ai AFTER INSERT ON market_listings BEGIN
    INSERT INTO market_fts(rowid, name, description, tags)
    VALUES (new.rowid, new.name, new.description, new.tags);
END;

CREATE TRIGGER market_listings_ad AFTER DELETE ON market_listings BEGIN
    INSERT INTO market_fts(market_fts, rowid, name, description, tags)
    VALUES ('delete', old.rowid, old.name, old.description, old.tags);
END;

CREATE TRIGGER market_listings_au AFTER UPDATE ON market_listings BEGIN
    INSERT INTO market_fts(market_fts, rowid, name, description, tags)
    VALUES ('delete', old.rowid, old.name, old.description, old.tags);
    INSERT INTO market_fts(rowid, name, description, tags)
    VALUES (new.rowid, new.name, new.description, new.tags);
END;

-- Ratings are intentionally not linked to market_listings: they must survive
-- an index refresh that temporarily drops a listing.
CREATE TABLE market_ratings (
    listing_id         TEXT PRIMARY KEY NOT NULL,
    score              INTEGER NOT NULL CHECK (score BETWEEN 1 AND 5),
    github_reaction_id TEXT,
    created_at         INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE trusted_publishers (
    key_id     TEXT PRIMARY KEY NOT NULL,
    publisher  TEXT NOT NULL,
    public_key TEXT NOT NULL,
    added_at   INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE scan_results (
    id         INTEGER PRIMARY KEY,
    plugin_id  TEXT NOT NULL,
    sha256     TEXT NOT NULL,
    verdict    TEXT NOT NULL CHECK (verdict IN ('clean', 'suspicious', 'blocked')),
    findings   TEXT NOT NULL DEFAULT '[]',
    scanned_at INTEGER NOT NULL,
    UNIQUE (plugin_id, sha256)
);

CREATE TABLE hotkeys (
    action      TEXT PRIMARY KEY NOT NULL,
    accelerator TEXT NOT NULL UNIQUE,
    enabled     INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1))
) WITHOUT ROWID;

CREATE TABLE perf_samples (
    id     INTEGER PRIMARY KEY,
    ts     INTEGER NOT NULL,
    source TEXT NOT NULL,
    metric TEXT NOT NULL,
    value  REAL NOT NULL
);
CREATE INDEX idx_perf_samples_metric ON perf_samples(metric, ts);

CREATE TABLE kv_cache (
    key        TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,
    expires_at INTEGER
) WITHOUT ROWID;
CREATE INDEX idx_kv_cache_expiry ON kv_cache(expires_at) WHERE expires_at IS NOT NULL;
