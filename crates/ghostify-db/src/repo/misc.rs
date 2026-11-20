//! Smaller tables: trusted publishers, scanner verdicts, hotkeys,
//! performance samples and the TTL cache.

use super::{col, now};
use crate::db::SqlExt;
use ghostify_core::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

// ---- trusted publishers --------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publisher {
    pub key_id: String,
    pub publisher: String,
    pub public_key: String,
    pub added_at: i64,
}

/// Trusts a signing key. Re-adding a known key id updates publisher and key.
pub fn add_publisher(
    conn: &Connection,
    key_id: &str,
    publisher: &str,
    public_key: &str,
) -> Result<Publisher> {
    if key_id.trim().is_empty() || publisher.trim().is_empty() || public_key.trim().is_empty() {
        return Err(Error::Invalid(
            "key id, publisher and public key must not be empty".into(),
        ));
    }
    let added_at = now();
    conn.execute(
        "INSERT INTO trusted_publishers(key_id, publisher, public_key, added_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(key_id) DO UPDATE SET
             publisher = excluded.publisher, public_key = excluded.public_key, added_at = excluded.added_at",
        params![key_id, publisher, public_key, added_at],
    )
    .db()?;
    Ok(Publisher {
        key_id: key_id.to_owned(),
        publisher: publisher.to_owned(),
        public_key: public_key.to_owned(),
        added_at,
    })
}

pub fn get_publisher(conn: &Connection, key_id: &str) -> Result<Option<Publisher>> {
    conn.query_row(
        "SELECT key_id, publisher, public_key, added_at FROM trusted_publishers WHERE key_id = ?1",
        [key_id],
        |r| {
            Ok(Publisher {
                key_id: r.get(0)?,
                publisher: r.get(1)?,
                public_key: r.get(2)?,
                added_at: r.get(3)?,
            })
        },
    )
    .optional()
    .db()
}

pub fn list_publishers(conn: &Connection) -> Result<Vec<Publisher>> {
    let mut stmt = conn
        .prepare("SELECT key_id, publisher, public_key, added_at FROM trusted_publishers ORDER BY publisher, key_id")
        .db()?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Publisher {
                key_id: r.get(0)?,
                publisher: r.get(1)?,
                public_key: r.get(2)?,
                added_at: r.get(3)?,
            })
        })
        .db()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().db()
}

pub fn remove_publisher(conn: &Connection, key_id: &str) -> Result<bool> {
    Ok(conn
        .execute("DELETE FROM trusted_publishers WHERE key_id = ?1", [key_id])
        .db()?
        > 0)
}

// ---- scanner verdicts ----------------------------------------------------

string_enum! {
    ScanVerdict {
        Clean => "clean",
        Suspicious => "suspicious",
        Blocked => "blocked",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanResult {
    pub id: i64,
    pub plugin_id: String,
    pub sha256: String,
    pub verdict: ScanVerdict,
    pub findings: Vec<String>,
    pub scanned_at: i64,
}

fn map_scan(row: &Row<'_>) -> rusqlite::Result<ScanResult> {
    let findings: String = row.get(4)?;
    Ok(ScanResult {
        id: row.get(0)?,
        plugin_id: row.get(1)?,
        sha256: row.get(2)?,
        verdict: col(ScanVerdict::parse(&row.get::<_, String>(3)?))?,
        findings: col(serde_json::from_str(&findings).map_err(Error::from))?,
        scanned_at: row.get(5)?,
    })
}

/// Stores the verdict for one exact build (plugin id plus content hash).
pub fn record_scan(
    conn: &Connection,
    plugin_id: &str,
    sha256: &str,
    verdict: ScanVerdict,
    findings: &[String],
) -> Result<ScanResult> {
    let findings = serde_json::to_string(findings)?;
    conn.query_row(
        "INSERT INTO scan_results(plugin_id, sha256, verdict, findings, scanned_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(plugin_id, sha256) DO UPDATE SET
             verdict = excluded.verdict, findings = excluded.findings, scanned_at = excluded.scanned_at
         RETURNING id, plugin_id, sha256, verdict, findings, scanned_at",
        params![plugin_id, sha256, verdict.as_str(), findings, now()],
        map_scan,
    )
    .db()
}

pub fn latest_scan(conn: &Connection, plugin_id: &str, sha256: &str) -> Result<Option<ScanResult>> {
    conn.query_row(
        "SELECT id, plugin_id, sha256, verdict, findings, scanned_at FROM scan_results
         WHERE plugin_id = ?1 AND sha256 = ?2",
        params![plugin_id, sha256],
        map_scan,
    )
    .optional()
    .db()
}

// ---- hotkeys ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    pub action: String,
    pub accelerator: String,
    pub enabled: bool,
}

/// Binds an accelerator to an action. An accelerator can only belong to one
/// action; binding a taken one is a conflict.
pub fn set_hotkey(
    conn: &Connection,
    action: &str,
    accelerator: &str,
    enabled: bool,
) -> Result<Hotkey> {
    if action.trim().is_empty() || accelerator.trim().is_empty() {
        return Err(Error::Invalid(
            "action and accelerator must not be empty".into(),
        ));
    }
    conn.execute(
        "INSERT INTO hotkeys(action, accelerator, enabled) VALUES (?1, ?2, ?3)
         ON CONFLICT(action) DO UPDATE SET accelerator = excluded.accelerator, enabled = excluded.enabled",
        params![action, accelerator, enabled as i64],
    )
    .map_err(|e| match crate::db::map_err(e) {
        Error::Conflict(_) => Error::Conflict(format!("accelerator `{accelerator}` is already in use")),
        other => other,
    })?;
    Ok(Hotkey {
        action: action.to_owned(),
        accelerator: accelerator.to_owned(),
        enabled,
    })
}

pub fn list_hotkeys(conn: &Connection) -> Result<Vec<Hotkey>> {
    let mut stmt = conn
        .prepare("SELECT action, accelerator, enabled FROM hotkeys ORDER BY action")
        .db()?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Hotkey {
                action: r.get(0)?,
                accelerator: r.get(1)?,
                enabled: r.get::<_, i64>(2)? != 0,
            })
        })
        .db()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().db()
}

pub fn remove_hotkey(conn: &Connection, action: &str) -> Result<bool> {
    Ok(conn
        .execute("DELETE FROM hotkeys WHERE action = ?1", [action])
        .db()?
        > 0)
}

// ---- performance samples ---------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerfSample {
    pub ts: i64,
    pub source: String,
    pub metric: String,
    pub value: f64,
}

/// Stores a batch of samples in one transaction.
pub fn add_samples(conn: &Connection, samples: &[PerfSample]) -> Result<()> {
    let tx = conn.unchecked_transaction().db()?;
    {
        let mut stmt = tx
            .prepare("INSERT INTO perf_samples(ts, source, metric, value) VALUES (?1, ?2, ?3, ?4)")
            .db()?;
        for sample in samples {
            stmt.execute(params![
                sample.ts,
                sample.source,
                sample.metric,
                sample.value
            ])
            .db()?;
        }
    }
    tx.commit().db()
}

/// Samples of one metric since `since` (Unix seconds), oldest first.
pub fn samples(conn: &Connection, metric: &str, since: i64) -> Result<Vec<PerfSample>> {
    let mut stmt = conn
        .prepare(
            "SELECT ts, source, metric, value FROM perf_samples
             WHERE metric = ?1 AND ts >= ?2 ORDER BY ts, id",
        )
        .db()?;
    let rows = stmt
        .query_map(params![metric, since], |r| {
            Ok(PerfSample {
                ts: r.get(0)?,
                source: r.get(1)?,
                metric: r.get(2)?,
                value: r.get(3)?,
            })
        })
        .db()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().db()
}

/// Deletes samples older than `before` and returns how many were removed.
pub fn prune_samples(conn: &Connection, before: i64) -> Result<usize> {
    conn.execute("DELETE FROM perf_samples WHERE ts < ?1", [before])
        .db()
}

// ---- TTL cache -------------------------------------------------------------

/// Stores a value, optionally expiring after `ttl_secs` seconds.
pub fn cache_put(conn: &Connection, key: &str, value: &str, ttl_secs: Option<u32>) -> Result<()> {
    let expires_at = ttl_secs.map(|ttl| now() + i64::from(ttl));
    conn.execute(
        "INSERT INTO kv_cache(key, value, expires_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, expires_at = excluded.expires_at",
        params![key, value, expires_at],
    )
    .db()?;
    Ok(())
}

/// Returns the value unless it has expired.
pub fn cache_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM kv_cache WHERE key = ?1 AND (expires_at IS NULL OR expires_at > ?2)",
        params![key, now()],
        |r| r.get(0),
    )
    .optional()
    .db()
}

pub fn cache_delete(conn: &Connection, key: &str) -> Result<bool> {
    Ok(conn
        .execute("DELETE FROM kv_cache WHERE key = ?1", [key])
        .db()?
        > 0)
}

/// Removes expired entries and returns how many were deleted.
pub fn cache_purge_expired(conn: &Connection) -> Result<usize> {
    conn.execute(
        "DELETE FROM kv_cache WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        [now()],
    )
    .db()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;

    #[test]
    fn publishers_are_upserted_listed_and_removed() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            add_publisher(c, "key-1", "Alice", "pub-a")?;
            add_publisher(c, "key-2", "Bob", "pub-b")?;
            add_publisher(c, "key-1", "Alice Inc", "pub-a2")?;
            let all = list_publishers(c)?;
            assert_eq!(all.len(), 2);
            assert_eq!(all[0].publisher, "Alice Inc");
            assert_eq!(get_publisher(c, "key-1")?.unwrap().public_key, "pub-a2");
            assert!(remove_publisher(c, "key-2")?);
            assert_eq!(get_publisher(c, "key-2")?, None);
            assert_eq!(
                add_publisher(c, "", "x", "y").unwrap_err().code(),
                "invalid_input"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn scan_results_are_keyed_by_plugin_and_hash() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            let first = record_scan(c, "a/b", "h1", ScanVerdict::Clean, &[])?;
            let second = record_scan(
                c,
                "a/b",
                "h1",
                ScanVerdict::Suspicious,
                &["uses eval".to_owned(), "undeclared host".to_owned()],
            )?;
            assert_eq!(first.id, second.id);
            let stored = latest_scan(c, "a/b", "h1")?.unwrap();
            assert_eq!(stored.verdict, ScanVerdict::Suspicious);
            assert_eq!(stored.findings, ["uses eval", "undeclared host"]);
            assert_eq!(latest_scan(c, "a/b", "h2")?, None);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn hotkeys_reject_duplicate_accelerators() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            set_hotkey(c, "player.toggle", "Ctrl+Alt+P", true)?;
            let err = set_hotkey(c, "player.next", "Ctrl+Alt+P", true).unwrap_err();
            assert_eq!(err.code(), "conflict");
            set_hotkey(c, "player.next", "Ctrl+Alt+N", true)?;
            set_hotkey(c, "player.toggle", "Ctrl+Shift+P", false)?;
            let keys = list_hotkeys(c)?;
            assert_eq!(keys.len(), 2);
            assert_eq!(
                keys[1],
                Hotkey {
                    action: "player.toggle".into(),
                    accelerator: "Ctrl+Shift+P".into(),
                    enabled: false
                }
            );
            assert!(remove_hotkey(c, "player.next")?);
            assert!(!remove_hotkey(c, "player.next")?);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn perf_samples_are_filtered_and_pruned() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            let batch: Vec<PerfSample> = (0..5)
                .map(|i| PerfSample {
                    ts: 100 + i,
                    source: "core".into(),
                    metric: "cpu".into(),
                    value: i as f64,
                })
                .chain([PerfSample {
                    ts: 100,
                    source: "core".into(),
                    metric: "ram".into(),
                    value: 1.0,
                }])
                .collect();
            add_samples(c, &batch)?;
            let recent = samples(c, "cpu", 102)?;
            assert_eq!(
                recent.iter().map(|s| s.value).collect::<Vec<_>>(),
                [2.0, 3.0, 4.0]
            );
            assert_eq!(prune_samples(c, 103)?, 4);
            assert_eq!(samples(c, "cpu", 0)?.len(), 2);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn cache_honours_ttl() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            cache_put(c, "forever", "v1", None)?;
            cache_put(c, "short", "v2", Some(3600))?;
            assert_eq!(cache_get(c, "forever")?.as_deref(), Some("v1"));
            assert_eq!(cache_get(c, "short")?.as_deref(), Some("v2"));

            // Force-expire an entry the way time passing would.
            c.execute("UPDATE kv_cache SET expires_at = 1 WHERE key = 'short'", [])
                .db()?;
            assert_eq!(cache_get(c, "short")?, None);
            assert_eq!(cache_purge_expired(c)?, 1);
            assert!(cache_delete(c, "forever")?);
            assert!(!cache_delete(c, "forever")?);
            Ok(())
        })
        .unwrap();
    }
}
