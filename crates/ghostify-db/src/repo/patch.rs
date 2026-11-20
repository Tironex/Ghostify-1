//! Patch state and history per Spotify installation.

use super::{col, now};
use crate::db::SqlExt;
use ghostify_core::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

string_enum! {
    PatchStatus {
        Unpatched => "unpatched",
        Patched => "patched",
        Failed => "failed",
        Restored => "restored",
    }
}

string_enum! {
    PatchAction {
        Patch => "patch",
        Restore => "restore",
        Repair => "repair",
        Repatch => "repatch",
    }
}

string_enum! {
    PatchOutcome {
        Success => "success",
        Failure => "failure",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchState {
    pub install_id: i64,
    pub status: PatchStatus,
    pub applied_rules_version: Option<String>,
    pub spotify_version: Option<String>,
    pub applied_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchHistoryEntry {
    pub id: i64,
    pub install_id: i64,
    pub action: PatchAction,
    pub outcome: PatchOutcome,
    pub error: Option<String>,
    pub created_at: i64,
}

fn map_state(row: &Row<'_>) -> rusqlite::Result<PatchState> {
    Ok(PatchState {
        install_id: row.get(0)?,
        status: col(PatchStatus::parse(&row.get::<_, String>(1)?))?,
        applied_rules_version: row.get(2)?,
        spotify_version: row.get(3)?,
        applied_at: row.get(4)?,
    })
}

fn map_history(row: &Row<'_>) -> rusqlite::Result<PatchHistoryEntry> {
    Ok(PatchHistoryEntry {
        id: row.get(0)?,
        install_id: row.get(1)?,
        action: col(PatchAction::parse(&row.get::<_, String>(2)?))?,
        outcome: col(PatchOutcome::parse(&row.get::<_, String>(3)?))?,
        error: row.get(4)?,
        created_at: row.get(5)?,
    })
}

/// Sets the patch state of an installation. `applied_at` is stamped when the
/// status is [`PatchStatus::Patched`].
pub fn set_state(
    conn: &Connection,
    install_id: i64,
    status: PatchStatus,
    rules_version: Option<&str>,
    spotify_version: Option<&str>,
) -> Result<PatchState> {
    let applied_at = (status == PatchStatus::Patched).then(now);
    conn.execute(
        "INSERT INTO patch_state(install_id, status, applied_rules_version, spotify_version, applied_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(install_id) DO UPDATE SET
             status = excluded.status,
             applied_rules_version = excluded.applied_rules_version,
             spotify_version = excluded.spotify_version,
             applied_at = excluded.applied_at",
        params![install_id, status.as_str(), rules_version, spotify_version, applied_at],
    )
    .db()?;
    Ok(PatchState {
        install_id,
        status,
        applied_rules_version: rules_version.map(str::to_owned),
        spotify_version: spotify_version.map(str::to_owned),
        applied_at,
    })
}

pub fn get_state(conn: &Connection, install_id: i64) -> Result<Option<PatchState>> {
    conn.query_row(
        "SELECT install_id, status, applied_rules_version, spotify_version, applied_at
         FROM patch_state WHERE install_id = ?1",
        [install_id],
        map_state,
    )
    .optional()
    .db()
}

pub fn record_history(
    conn: &Connection,
    install_id: i64,
    action: PatchAction,
    outcome: PatchOutcome,
    error: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO patch_history(install_id, action, outcome, error, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![install_id, action.as_str(), outcome.as_str(), error, now()],
    )
    .db()?;
    Ok(conn.last_insert_rowid())
}

/// Newest entries first.
pub fn history(conn: &Connection, install_id: i64, limit: u32) -> Result<Vec<PatchHistoryEntry>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, install_id, action, outcome, error, created_at
             FROM patch_history WHERE install_id = ?1 ORDER BY id DESC LIMIT ?2",
        )
        .db()?;
    let rows = stmt
        .query_map(params![install_id, limit], map_history)
        .db()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().db()
}

/// Keeps only the newest `keep` entries of an installation and returns how many were removed.
pub fn prune_history(conn: &Connection, install_id: i64, keep: u32) -> Result<usize> {
    conn.execute(
        "DELETE FROM patch_history WHERE install_id = ?1 AND id NOT IN (
             SELECT id FROM patch_history WHERE install_id = ?1 ORDER BY id DESC LIMIT ?2)",
        params![install_id, keep],
    )
    .db()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::installs::{self, InstallKind};
    use crate::Db;

    fn install(db: &Db) -> i64 {
        db.with(|c| installs::upsert(c, InstallKind::LinuxNative, "/opt/spotify", Some("1.0")))
            .unwrap()
            .id
    }

    #[test]
    fn state_transitions() {
        let db = Db::open_in_memory().unwrap();
        let id = install(&db);
        db.with(|c| {
            assert_eq!(get_state(c, id)?, None);
            let patched = set_state(c, id, PatchStatus::Patched, Some("2026.11.1"), Some("1.0"))?;
            assert!(patched.applied_at.is_some());
            assert_eq!(get_state(c, id)?.unwrap(), patched);
            let restored = set_state(c, id, PatchStatus::Restored, None, Some("1.0"))?;
            assert_eq!(restored.applied_at, None);
            assert_eq!(get_state(c, id)?.unwrap().status, PatchStatus::Restored);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn state_requires_an_existing_installation() {
        let db = Db::open_in_memory().unwrap();
        let err = db
            .with(|c| set_state(c, 999, PatchStatus::Patched, None, None))
            .unwrap_err();
        assert_eq!(err.code(), "conflict");
    }

    #[test]
    fn history_is_newest_first_and_prunable() {
        let db = Db::open_in_memory().unwrap();
        let id = install(&db);
        db.with(|c| {
            for i in 0..5 {
                let error = format!("failure {i}");
                record_history(
                    c,
                    id,
                    PatchAction::Repatch,
                    PatchOutcome::Failure,
                    Some(&error),
                )?;
            }
            record_history(c, id, PatchAction::Patch, PatchOutcome::Success, None)?;
            let entries = history(c, id, 3)?;
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].action, PatchAction::Patch);
            assert_eq!(entries[1].error.as_deref(), Some("failure 4"));
            assert_eq!(prune_history(c, id, 2)?, 4);
            assert_eq!(history(c, id, 10)?.len(), 2);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn removing_the_installation_cascades() {
        let db = Db::open_in_memory().unwrap();
        let id = install(&db);
        db.with(|c| {
            set_state(c, id, PatchStatus::Patched, None, None)?;
            record_history(c, id, PatchAction::Patch, PatchOutcome::Success, None)?;
            installs::remove(c, id)?;
            assert_eq!(get_state(c, id)?, None);
            assert!(history(c, id, 10)?.is_empty());
            Ok(())
        })
        .unwrap();
    }
}
