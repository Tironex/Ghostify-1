//! Database handle: connection setup, error mapping and async access.

use crate::migrate;
use ghostify_core::{Error, Result};
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Converts a SQLite error into the shared error type. Constraint violations
/// (uniqueness, CHECK, foreign keys) become [`Error::Conflict`].
pub fn map_err(err: rusqlite::Error) -> Error {
    match &err {
        rusqlite::Error::QueryReturnedNoRows => Error::NotFound("row".into()),
        rusqlite::Error::SqliteFailure(failure, message)
            if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Error::Conflict(message.clone().unwrap_or_else(|| err.to_string()))
        }
        _ => Error::Database(err.to_string()),
    }
}

/// Adds `.db()` to rusqlite results.
pub trait SqlExt<T> {
    fn db(self) -> Result<T>;
}

impl<T> SqlExt<T> for rusqlite::Result<T> {
    fn db(self) -> Result<T> {
        self.map_err(map_err)
    }
}

/// Cloneable handle to the Ghostify database.
///
/// A single connection in WAL mode serves the whole application; all work is
/// short, and one connection keeps memory use and lock contention minimal.
/// Use [`Db::call`] from async code so that Tokio worker threads never block.
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").finish_non_exhaustive()
    }
}

impl Db {
    /// Opens (creating if necessary) the database file and migrates it to the
    /// latest schema.
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).db()?;
        Db::init(conn, true)
    }

    /// In-memory database, used by tests and ephemeral sessions.
    pub fn open_in_memory() -> Result<Db> {
        let conn = Connection::open_in_memory().db()?;
        Db::init(conn, false)
    }

    fn init(mut conn: Connection, wal: bool) -> Result<Db> {
        conn.busy_timeout(Duration::from_secs(5)).db()?;
        conn.pragma_update(None, "foreign_keys", "ON").db()?;
        if wal {
            conn.pragma_update(None, "journal_mode", "WAL").db()?;
        }
        conn.pragma_update(None, "synchronous", "NORMAL").db()?;
        conn.pragma_update(None, "temp_store", "MEMORY").db()?;
        // Negative values are KiB: an 8 MiB page cache keeps the footprint small.
        conn.pragma_update(None, "cache_size", -8192).db()?;
        migrate::run(&mut conn)?;
        Ok(Db {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Runs `f` with the connection on the current thread.
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&guard)
    }

    /// Runs `f` on Tokio's blocking pool.
    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.with(f))
            .await
            .map_err(|e| Error::Internal(format!("database task failed: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_database_uses_wal_and_foreign_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("nested").join("ghostify.db")).unwrap();
        db.with(|c| {
            let mode: String = c.query_row("PRAGMA journal_mode", [], |r| r.get(0)).db()?;
            assert_eq!(mode, "wal");
            let fk: i64 = c.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).db()?;
            assert_eq!(fk, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn reopening_keeps_data_and_does_not_remigrate() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ghostify.db");
        let db = Db::open(&path).unwrap();
        db.with(|c| crate::repo::settings::set(c, "k", &42))
            .unwrap();
        drop(db);
        let db = Db::open(&path).unwrap();
        let value: Option<i64> = db.with(|c| crate::repo::settings::get(c, "k")).unwrap();
        assert_eq!(value, Some(42));
    }

    #[tokio::test]
    async fn async_call_runs_on_blocking_pool() {
        let db = Db::open_in_memory().unwrap();
        db.call(|c| crate::repo::settings::set(c, "answer", &"yes"))
            .await
            .unwrap();
        let value: Option<String> = db
            .call(|c| crate::repo::settings::get(c, "answer"))
            .await
            .unwrap();
        assert_eq!(value.as_deref(), Some("yes"));
    }

    #[test]
    fn constraint_violations_map_to_conflict() {
        let db = Db::open_in_memory().unwrap();
        let err = db
            .with(|c| {
                c.execute(
                    "INSERT INTO plugins(id,name,version,source,runtime,installed_at,updated_at)
                     VALUES ('a','a','1','bogus','sandboxed',0,0)",
                    [],
                )
                .db()
            })
            .unwrap_err();
        assert_eq!(err.code(), "conflict");
    }
}
