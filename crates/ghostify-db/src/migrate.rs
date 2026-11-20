//! Schema migrations tracked through `PRAGMA user_version`.

use crate::db::SqlExt;
use ghostify_core::{Error, Result};
use rusqlite::Connection;

/// Ordered, immutable migrations. A released migration is never edited; schema
/// changes are added as a new entry.
const MIGRATIONS: &[(u32, &str)] = &[
    (1, include_str!("../migrations/001_init.sql")),
    (2, include_str!("../migrations/002_market_packages.sql")),
];

/// Schema version this build of Ghostify expects.
pub fn latest_version() -> u32 {
    MIGRATIONS.last().map(|(version, _)| *version).unwrap_or(0)
}

/// Current schema version of the database.
pub fn current_version(conn: &Connection) -> Result<u32> {
    conn.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
        .db()
}

/// Applies all pending migrations, each in its own transaction. A database
/// created by a newer Ghostify is rejected instead of being modified.
pub fn run(conn: &mut Connection) -> Result<()> {
    let current = current_version(conn)?;
    let latest = latest_version();
    if current > latest {
        return Err(Error::Database(format!(
            "database schema version {current} is newer than the supported version {latest}; update Ghostify"
        )));
    }
    for (version, sql) in MIGRATIONS.iter().filter(|(version, _)| *version > current) {
        let tx = conn.transaction().db()?;
        tx.execute_batch(sql)
            .map_err(|e| Error::Database(format!("migration {version} failed: {e}")))?;
        tx.pragma_update(None, "user_version", *version).db()?;
        tx.commit().db()?;
        tracing::info!(version, "applied database migration");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_database_is_migrated_to_latest() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(current_version(&conn).unwrap(), 0);
        run(&mut conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), latest_version());
        run(&mut conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), latest_version());
    }

    #[test]
    fn all_expected_tables_exist() {
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn).unwrap();
        for table in [
            "settings",
            "spotify_installs",
            "patch_state",
            "patch_history",
            "plugins",
            "plugin_permissions",
            "plugin_snapshots",
            "plugin_events",
            "themes",
            "market_listings",
            "market_fts",
            "market_ratings",
            "trusted_publishers",
            "scan_results",
            "hotkeys",
            "perf_samples",
            "kv_cache",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table {table} missing");
        }
    }

    #[test]
    fn newer_database_is_rejected() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", latest_version() + 1)
            .unwrap();
        assert_eq!(run(&mut conn).unwrap_err().code(), "database");
    }
}
