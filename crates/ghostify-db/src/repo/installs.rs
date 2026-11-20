//! Detected Spotify installations.

use super::{col, now};
use crate::db::SqlExt;
use ghostify_core::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

string_enum! {
    /// How Spotify was installed; decides where its files live and how they are patched.
    InstallKind {
        WindowsDesktop => "windows_desktop",
        WindowsStore => "windows_store",
        Macos => "macos",
        LinuxNative => "linux_native",
        LinuxFlatpak => "linux_flatpak",
        LinuxSnap => "linux_snap",
        Custom => "custom",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpotifyInstall {
    pub id: i64,
    pub kind: InstallKind,
    pub root_path: String,
    pub version: Option<String>,
    pub last_seen_at: i64,
}

const COLUMNS: &str = "id, kind, root_path, version, last_seen_at";

fn map(row: &Row<'_>) -> rusqlite::Result<SpotifyInstall> {
    Ok(SpotifyInstall {
        id: row.get(0)?,
        kind: col(InstallKind::parse(&row.get::<_, String>(1)?))?,
        root_path: row.get(2)?,
        version: row.get(3)?,
        last_seen_at: row.get(4)?,
    })
}

/// Records an installation, or refreshes version and `last_seen_at` of a known one.
pub fn upsert(
    conn: &Connection,
    kind: InstallKind,
    root_path: &str,
    version: Option<&str>,
) -> Result<SpotifyInstall> {
    if root_path.trim().is_empty() {
        return Err(Error::Invalid("root_path must not be empty".into()));
    }
    conn.query_row(
        &format!(
            "INSERT INTO spotify_installs(kind, root_path, version, last_seen_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(root_path) DO UPDATE SET
                 kind = excluded.kind, version = excluded.version, last_seen_at = excluded.last_seen_at
             RETURNING {COLUMNS}"
        ),
        params![kind.as_str(), root_path, version, now()],
        map,
    )
    .db()
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<SpotifyInstall>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM spotify_installs WHERE id = ?1"),
        [id],
        map,
    )
    .optional()
    .db()
}

pub fn list(conn: &Connection) -> Result<Vec<SpotifyInstall>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM spotify_installs ORDER BY id"
        ))
        .db()?;
    let rows = stmt.query_map([], map).db()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().db()
}

pub fn remove(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn
        .execute("DELETE FROM spotify_installs WHERE id = ?1", [id])
        .db()?
        > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;

    #[test]
    fn upsert_refreshes_existing_installation() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            let first = upsert(c, InstallKind::LinuxNative, "/opt/spotify", Some("1.2.3"))?;
            let second = upsert(c, InstallKind::LinuxNative, "/opt/spotify", Some("1.2.4"))?;
            assert_eq!(first.id, second.id);
            assert_eq!(second.version.as_deref(), Some("1.2.4"));
            upsert(
                c,
                InstallKind::LinuxFlatpak,
                "/var/lib/flatpak/spotify",
                None,
            )?;
            assert_eq!(list(c)?.len(), 2);
            assert_eq!(get(c, first.id)?.unwrap().root_path, "/opt/spotify");
            assert!(remove(c, first.id)?);
            assert_eq!(get(c, first.id)?, None);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn empty_path_is_invalid() {
        let db = Db::open_in_memory().unwrap();
        let err = db
            .with(|c| upsert(c, InstallKind::Custom, "  ", None))
            .unwrap_err();
        assert_eq!(err.code(), "invalid_input");
    }

    #[test]
    fn kind_round_trips_through_text() {
        for kind in [
            InstallKind::WindowsDesktop,
            InstallKind::Macos,
            InstallKind::LinuxSnap,
        ] {
            assert_eq!(InstallKind::parse(kind.as_str()).unwrap(), kind);
        }
        assert!(InstallKind::parse("amiga").is_err());
    }
}
