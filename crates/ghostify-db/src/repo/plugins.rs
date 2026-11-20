//! Installed plugins, their permissions, rollback snapshots and the
//! lifecycle/crash log.

use super::{col, now};
use crate::db::SqlExt;
use ghostify_core::{Error, PluginState, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

string_enum! {
    PluginSource {
        Marketplace => "marketplace",
        Github => "github",
        Local => "local",
    }
}

string_enum! {
    /// `Sandboxed` plugins run isolated; `Legacy` plugins (Spicetify
    /// compatibility) have full renderer access.
    PluginRuntime {
        Sandboxed => "sandboxed",
        Legacy => "legacy",
    }
}

string_enum! {
    PluginEventKind {
        Loaded => "loaded",
        Activated => "activated",
        Deactivated => "deactivated",
        Updated => "updated",
        Crash => "crash",
        Timeout => "timeout",
        Rollback => "rollback",
        Disabled => "disabled",
    }
}

fn state_str(state: PluginState) -> &'static str {
    match state {
        PluginState::Installed => "installed",
        PluginState::Loaded => "loaded",
        PluginState::Active => "active",
        PluginState::Suspended => "suspended",
        PluginState::Crashed => "crashed",
        PluginState::Unloaded => "unloaded",
        PluginState::Removed => "removed",
    }
}

fn parse_state(value: &str) -> Result<PluginState> {
    Ok(match value {
        "installed" => PluginState::Installed,
        "loaded" => PluginState::Loaded,
        "active" => PluginState::Active,
        "suspended" => PluginState::Suspended,
        "crashed" => PluginState::Crashed,
        "unloaded" => PluginState::Unloaded,
        "removed" => PluginState::Removed,
        other => return Err(Error::Invalid(format!("unknown plugin state `{other}`"))),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub source: PluginSource,
    pub runtime: PluginRuntime,
    pub enabled: bool,
    pub state: PluginState,
    pub sha256: Option<String>,
    pub installed_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub source: PluginSource,
    pub runtime: PluginRuntime,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: i64,
    pub plugin_id: String,
    pub version: String,
    pub path: String,
    pub sha256: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginEvent {
    pub id: i64,
    pub plugin_id: String,
    pub kind: PluginEventKind,
    pub detail: Option<String>,
    pub created_at: i64,
}

/// Outcome of [`record_crash`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashVerdict {
    /// Crashes and timeouts inside the observation window, including this one.
    pub crashes_in_window: u32,
    /// The threshold was reached and the plugin has been disabled.
    pub disabled: bool,
    /// Snapshot of the last other version, to roll back to. Only set when `disabled`.
    pub rollback: Option<Snapshot>,
}

const PLUGIN_COLUMNS: &str =
    "id, name, version, source, runtime, enabled, state, sha256, installed_at, updated_at";

fn map_plugin(row: &Row<'_>) -> rusqlite::Result<Plugin> {
    Ok(Plugin {
        id: row.get(0)?,
        name: row.get(1)?,
        version: row.get(2)?,
        source: col(PluginSource::parse(&row.get::<_, String>(3)?))?,
        runtime: col(PluginRuntime::parse(&row.get::<_, String>(4)?))?,
        enabled: row.get::<_, i64>(5)? != 0,
        state: col(parse_state(&row.get::<_, String>(6)?))?,
        sha256: row.get(7)?,
        installed_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn map_snapshot(row: &Row<'_>) -> rusqlite::Result<Snapshot> {
    Ok(Snapshot {
        id: row.get(0)?,
        plugin_id: row.get(1)?,
        version: row.get(2)?,
        path: row.get(3)?,
        sha256: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn map_event(row: &Row<'_>) -> rusqlite::Result<PluginEvent> {
    Ok(PluginEvent {
        id: row.get(0)?,
        plugin_id: row.get(1)?,
        kind: col(PluginEventKind::parse(&row.get::<_, String>(2)?))?,
        detail: row.get(3)?,
        created_at: row.get(4)?,
    })
}

/// Registers a new plugin. It starts disabled, without permissions.
pub fn install(conn: &Connection, new: &NewPlugin) -> Result<Plugin> {
    if new.id.trim().is_empty() || new.name.trim().is_empty() || new.version.trim().is_empty() {
        return Err(Error::Invalid(
            "plugin id, name and version must not be empty".into(),
        ));
    }
    let ts = now();
    conn.execute(
        "INSERT INTO plugins(id, name, version, source, runtime, enabled, state, sha256, installed_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, 'installed', ?6, ?7, ?7)",
        params![new.id, new.name, new.version, new.source.as_str(), new.runtime.as_str(), new.sha256, ts],
    )
    .map_err(|e| match crate::db::map_err(e) {
        Error::Conflict(_) => Error::Conflict(format!("plugin `{}` is already installed", new.id)),
        other => other,
    })?;
    require(conn, &new.id)
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Plugin>> {
    conn.query_row(
        &format!("SELECT {PLUGIN_COLUMNS} FROM plugins WHERE id = ?1"),
        [id],
        map_plugin,
    )
    .optional()
    .db()
}

fn require(conn: &Connection, id: &str) -> Result<Plugin> {
    get(conn, id)?.ok_or_else(|| Error::NotFound(format!("plugin `{id}`")))
}

pub fn list(conn: &Connection) -> Result<Vec<Plugin>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {PLUGIN_COLUMNS} FROM plugins ORDER BY name COLLATE NOCASE, id"
        ))
        .db()?;
    let rows = stmt.query_map([], map_plugin).db()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().db()
}

fn touched(changed: usize, id: &str) -> Result<()> {
    if changed == 0 {
        Err(Error::NotFound(format!("plugin `{id}`")))
    } else {
        Ok(())
    }
}

pub fn set_enabled(conn: &Connection, id: &str, enabled: bool) -> Result<Plugin> {
    let changed = conn
        .execute(
            "UPDATE plugins SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, enabled as i64, now()],
        )
        .db()?;
    touched(changed, id)?;
    require(conn, id)
}

pub fn set_state(conn: &Connection, id: &str, state: PluginState) -> Result<Plugin> {
    let changed = conn
        .execute(
            "UPDATE plugins SET state = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, state_str(state), now()],
        )
        .db()?;
    touched(changed, id)?;
    require(conn, id)
}

/// Records a new installed version (after an update or rollback).
pub fn set_version(
    conn: &Connection,
    id: &str,
    version: &str,
    sha256: Option<&str>,
) -> Result<Plugin> {
    if version.trim().is_empty() {
        return Err(Error::Invalid("version must not be empty".into()));
    }
    let changed = conn
        .execute(
            "UPDATE plugins SET version = ?2, sha256 = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, version, sha256, now()],
        )
        .db()?;
    touched(changed, id)?;
    require(conn, id)
}

/// Removes a plugin with its permissions, snapshots and events. Returns the
/// snapshot paths so that the caller can delete the files.
pub fn remove(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let tx = conn.unchecked_transaction().db()?;
    let paths = snapshot_paths(&tx, id)?;
    let changed = tx.execute("DELETE FROM plugins WHERE id = ?1", [id]).db()?;
    touched(changed, id)?;
    tx.commit().db()?;
    Ok(paths)
}

fn snapshot_paths(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT path FROM plugin_snapshots WHERE plugin_id = ?1")
        .db()?;
    let rows = stmt.query_map([id], |r| r.get::<_, String>(0)).db()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().db()
}

// ---- permissions ---------------------------------------------------------

pub fn grant(conn: &Connection, plugin_id: &str, permission: &str) -> Result<()> {
    if permission.trim().is_empty() {
        return Err(Error::Invalid("permission must not be empty".into()));
    }
    conn.execute(
        "INSERT OR IGNORE INTO plugin_permissions(plugin_id, permission, granted_at) VALUES (?1, ?2, ?3)",
        params![plugin_id, permission, now()],
    )
    .map_err(|e| match crate::db::map_err(e) {
        Error::Conflict(_) => Error::NotFound(format!("plugin `{plugin_id}`")),
        other => other,
    })?;
    Ok(())
}

/// Returns `true` if the permission had been granted.
pub fn revoke(conn: &Connection, plugin_id: &str, permission: &str) -> Result<bool> {
    Ok(conn
        .execute(
            "DELETE FROM plugin_permissions WHERE plugin_id = ?1 AND permission = ?2",
            params![plugin_id, permission],
        )
        .db()?
        > 0)
}

/// Sorted list of granted permissions.
pub fn permissions(conn: &Connection, plugin_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT permission FROM plugin_permissions WHERE plugin_id = ?1 ORDER BY permission",
        )
        .db()?;
    let rows = stmt
        .query_map([plugin_id], |r| r.get::<_, String>(0))
        .db()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().db()
}

pub fn has_permission(conn: &Connection, plugin_id: &str, permission: &str) -> Result<bool> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM plugin_permissions WHERE plugin_id = ?1 AND permission = ?2",
            params![plugin_id, permission],
            |r| r.get(0),
        )
        .db()?;
    Ok(count > 0)
}

// ---- snapshots -----------------------------------------------------------

/// Stores (or replaces) the snapshot of a version.
pub fn add_snapshot(
    conn: &Connection,
    plugin_id: &str,
    version: &str,
    path: &str,
    sha256: &str,
) -> Result<Snapshot> {
    conn.query_row(
        "INSERT INTO plugin_snapshots(plugin_id, version, path, sha256, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(plugin_id, version) DO UPDATE SET
             path = excluded.path, sha256 = excluded.sha256, created_at = excluded.created_at
         RETURNING id, plugin_id, version, path, sha256, created_at",
        params![plugin_id, version, path, sha256, now()],
        map_snapshot,
    )
    .map_err(|e| match crate::db::map_err(e) {
        Error::Conflict(_) => Error::NotFound(format!("plugin `{plugin_id}`")),
        other => other,
    })
}

/// Newest snapshot of a version other than `current_version`.
pub fn previous_snapshot(
    conn: &Connection,
    plugin_id: &str,
    current_version: &str,
) -> Result<Option<Snapshot>> {
    conn.query_row(
        "SELECT id, plugin_id, version, path, sha256, created_at FROM plugin_snapshots
         WHERE plugin_id = ?1 AND version <> ?2 ORDER BY created_at DESC, id DESC LIMIT 1",
        params![plugin_id, current_version],
        map_snapshot,
    )
    .optional()
    .db()
}

pub fn snapshots(conn: &Connection, plugin_id: &str) -> Result<Vec<Snapshot>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, plugin_id, version, path, sha256, created_at FROM plugin_snapshots
             WHERE plugin_id = ?1 ORDER BY created_at DESC, id DESC",
        )
        .db()?;
    let rows = stmt.query_map([plugin_id], map_snapshot).db()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().db()
}

/// Keeps the newest `keep` snapshots and returns the paths of the deleted
/// ones so that the caller can remove the files.
pub fn prune_snapshots(conn: &Connection, plugin_id: &str, keep: u32) -> Result<Vec<String>> {
    let tx = conn.unchecked_transaction().db()?;
    let doomed: Vec<(i64, String)> = {
        let mut stmt = tx
            .prepare(
                "SELECT id, path FROM plugin_snapshots WHERE plugin_id = ?1
                 ORDER BY created_at DESC, id DESC LIMIT -1 OFFSET ?2",
            )
            .db()?;
        let rows = stmt
            .query_map(params![plugin_id, keep], |r| Ok((r.get(0)?, r.get(1)?)))
            .db()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().db()?
    };
    for (id, _) in &doomed {
        tx.execute("DELETE FROM plugin_snapshots WHERE id = ?1", [id])
            .db()?;
    }
    tx.commit().db()?;
    Ok(doomed.into_iter().map(|(_, path)| path).collect())
}

// ---- events and crash handling ------------------------------------------

pub fn record_event(
    conn: &Connection,
    plugin_id: &str,
    kind: PluginEventKind,
    detail: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO plugin_events(plugin_id, kind, detail, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![plugin_id, kind.as_str(), detail, now()],
    )
    .map_err(|e| match crate::db::map_err(e) {
        Error::Conflict(_) => Error::NotFound(format!("plugin `{plugin_id}`")),
        other => other,
    })?;
    Ok(conn.last_insert_rowid())
}

/// Newest events first.
pub fn recent_events(conn: &Connection, plugin_id: &str, limit: u32) -> Result<Vec<PluginEvent>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, plugin_id, kind, detail, created_at FROM plugin_events
             WHERE plugin_id = ?1 ORDER BY id DESC LIMIT ?2",
        )
        .db()?;
    let rows = stmt.query_map(params![plugin_id, limit], map_event).db()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().db()
}

/// Number of crashes and hook timeouts since the Unix timestamp `since`.
pub fn crash_count_since(conn: &Connection, plugin_id: &str, since: i64) -> Result<u32> {
    conn.query_row(
        "SELECT COUNT(*) FROM plugin_events
         WHERE plugin_id = ?1 AND kind IN ('crash', 'timeout') AND created_at >= ?2",
        params![plugin_id, since],
        |r| r.get(0),
    )
    .db()
}

/// Records a crash (or hook timeout) and applies the crash policy atomically:
/// when `threshold` crashes happened within `window_secs`, the plugin is
/// disabled, marked as crashed, and the snapshot to roll back to is returned.
pub fn record_crash(
    conn: &Connection,
    plugin_id: &str,
    kind: PluginEventKind,
    detail: Option<&str>,
    threshold: u32,
    window_secs: u64,
) -> Result<CrashVerdict> {
    if !matches!(kind, PluginEventKind::Crash | PluginEventKind::Timeout) {
        return Err(Error::Invalid(
            "record_crash accepts only crash and timeout events".into(),
        ));
    }
    let tx = conn.unchecked_transaction().db()?;
    let plugin = require(&tx, plugin_id)?;
    record_event(&tx, plugin_id, kind, detail)?;
    let since = now().saturating_sub(window_secs.min(i64::MAX as u64) as i64);
    let crashes = crash_count_since(&tx, plugin_id, since)?;
    let disabled = crashes >= threshold;
    let mut rollback = None;
    if disabled {
        tx.execute(
            "UPDATE plugins SET enabled = 0, state = 'crashed', updated_at = ?2 WHERE id = ?1",
            params![plugin_id, now()],
        )
        .db()?;
        record_event(
            &tx,
            plugin_id,
            PluginEventKind::Disabled,
            Some("crash threshold reached"),
        )?;
        rollback = previous_snapshot(&tx, plugin_id, &plugin.version)?;
    }
    tx.commit().db()?;
    Ok(CrashVerdict {
        crashes_in_window: crashes,
        disabled,
        rollback,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;

    fn new_plugin(id: &str) -> NewPlugin {
        NewPlugin {
            id: id.into(),
            name: format!("Plugin {id}"),
            version: "1.0.0".into(),
            source: PluginSource::Marketplace,
            runtime: PluginRuntime::Sandboxed,
            sha256: Some("abc".into()),
        }
    }

    #[test]
    fn install_get_update_remove() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            let plugin = install(c, &new_plugin("owner/one"))?;
            assert!(!plugin.enabled);
            assert_eq!(plugin.state, PluginState::Installed);
            assert_eq!(
                install(c, &new_plugin("owner/one")).unwrap_err().code(),
                "conflict"
            );

            assert!(set_enabled(c, "owner/one", true)?.enabled);
            assert_eq!(
                set_state(c, "owner/one", PluginState::Active)?.state,
                PluginState::Active
            );
            let updated = set_version(c, "owner/one", "1.1.0", Some("def"))?;
            assert_eq!(updated.version, "1.1.0");
            assert_eq!(updated.sha256.as_deref(), Some("def"));

            install(c, &new_plugin("owner/two"))?;
            let ids: Vec<_> = list(c)?.into_iter().map(|p| p.id).collect();
            assert_eq!(ids, ["owner/one", "owner/two"]);

            add_snapshot(c, "owner/one", "1.0.0", "/snap/1", "abc")?;
            assert_eq!(remove(c, "owner/one")?, ["/snap/1"]);
            assert_eq!(get(c, "owner/one")?, None);
            assert_eq!(remove(c, "owner/one").unwrap_err().code(), "not_found");
            assert_eq!(
                set_enabled(c, "ghost", true).unwrap_err().code(),
                "not_found"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn permissions_are_per_plugin_and_idempotent() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            install(c, &new_plugin("owner/one"))?;
            grant(c, "owner/one", "player.read")?;
            grant(c, "owner/one", "player.read")?;
            grant(c, "owner/one", "net")?;
            assert_eq!(permissions(c, "owner/one")?, ["net", "player.read"]);
            assert!(has_permission(c, "owner/one", "net")?);
            assert!(revoke(c, "owner/one", "net")?);
            assert!(!revoke(c, "owner/one", "net")?);
            assert!(!has_permission(c, "owner/one", "net")?);
            assert_eq!(grant(c, "ghost", "net").unwrap_err().code(), "not_found");
            assert_eq!(
                grant(c, "owner/one", " ").unwrap_err().code(),
                "invalid_input"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn snapshots_select_previous_version_and_prune() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            install(c, &new_plugin("owner/one"))?;
            for v in ["1.0.0", "1.1.0", "1.2.0"] {
                add_snapshot(c, "owner/one", v, &format!("/snap/{v}"), "hash")?;
            }
            let previous = previous_snapshot(c, "owner/one", "1.2.0")?.unwrap();
            assert_eq!(previous.version, "1.1.0");
            assert_eq!(previous_snapshot(c, "ghost", "1")?, None);

            // Re-adding a version replaces its snapshot instead of failing.
            add_snapshot(c, "owner/one", "1.0.0", "/snap/new-1.0.0", "hash2")?;
            assert_eq!(snapshots(c, "owner/one")?.len(), 3);

            let removed = prune_snapshots(c, "owner/one", 1)?;
            assert_eq!(removed.len(), 2);
            assert_eq!(snapshots(c, "owner/one")?.len(), 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn crashes_below_threshold_keep_the_plugin_enabled() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            install(c, &new_plugin("owner/one"))?;
            set_enabled(c, "owner/one", true)?;
            let verdict = record_crash(
                c,
                "owner/one",
                PluginEventKind::Crash,
                Some("TypeError"),
                3,
                600,
            )?;
            assert_eq!(
                verdict,
                CrashVerdict {
                    crashes_in_window: 1,
                    disabled: false,
                    rollback: None
                }
            );
            assert!(get(c, "owner/one")?.unwrap().enabled);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn reaching_the_threshold_disables_and_offers_rollback() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            install(c, &new_plugin("owner/one"))?;
            add_snapshot(c, "owner/one", "0.9.0", "/snap/0.9.0", "old")?;
            set_version(c, "owner/one", "1.0.1", None)?;
            set_enabled(c, "owner/one", true)?;

            record_crash(c, "owner/one", PluginEventKind::Crash, None, 3, 600)?;
            record_crash(
                c,
                "owner/one",
                PluginEventKind::Timeout,
                Some("onActivate"),
                3,
                600,
            )?;
            let verdict = record_crash(c, "owner/one", PluginEventKind::Crash, None, 3, 600)?;

            assert!(verdict.disabled);
            assert_eq!(verdict.crashes_in_window, 3);
            assert_eq!(verdict.rollback.unwrap().version, "0.9.0");
            let plugin = get(c, "owner/one")?.unwrap();
            assert!(!plugin.enabled);
            assert_eq!(plugin.state, PluginState::Crashed);
            let events = recent_events(c, "owner/one", 10)?;
            assert_eq!(events[0].kind, PluginEventKind::Disabled);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn crash_policy_rejects_other_event_kinds_and_unknown_plugins() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            install(c, &new_plugin("owner/one"))?;
            assert_eq!(
                record_crash(c, "owner/one", PluginEventKind::Loaded, None, 3, 600)
                    .unwrap_err()
                    .code(),
                "invalid_input"
            );
            assert_eq!(
                record_crash(c, "ghost", PluginEventKind::Crash, None, 3, 600)
                    .unwrap_err()
                    .code(),
                "not_found"
            );
            assert_eq!(crash_count_since(c, "owner/one", 0)?, 0);
            Ok(())
        })
        .unwrap();
    }
}
