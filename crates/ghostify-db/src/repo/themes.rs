//! Installed themes. At most one theme is active at a time (enforced by a
//! partial unique index).

use super::{col, now};
use crate::db::SqlExt;
use ghostify_core::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

string_enum! {
    ThemeSource {
        Marketplace => "marketplace",
        Github => "github",
        Local => "local",
        Import => "import",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub version: String,
    pub source: ThemeSource,
    pub active: bool,
    pub color_scheme: String,
    pub installed_at: i64,
}

const COLUMNS: &str = "id, name, version, source, active, color_scheme, installed_at";

fn map(row: &Row<'_>) -> rusqlite::Result<Theme> {
    Ok(Theme {
        id: row.get(0)?,
        name: row.get(1)?,
        version: row.get(2)?,
        source: col(ThemeSource::parse(&row.get::<_, String>(3)?))?,
        active: row.get::<_, i64>(4)? != 0,
        color_scheme: row.get(5)?,
        installed_at: row.get(6)?,
    })
}

pub fn install(
    conn: &Connection,
    id: &str,
    name: &str,
    version: &str,
    source: ThemeSource,
) -> Result<Theme> {
    if id.trim().is_empty() || name.trim().is_empty() || version.trim().is_empty() {
        return Err(Error::Invalid(
            "theme id, name and version must not be empty".into(),
        ));
    }
    conn.execute(
        "INSERT INTO themes(id, name, version, source, active, color_scheme, installed_at)
         VALUES (?1, ?2, ?3, ?4, 0, 'default', ?5)",
        params![id, name, version, source.as_str(), now()],
    )
    .map_err(|e| match crate::db::map_err(e) {
        Error::Conflict(_) => Error::Conflict(format!("theme `{id}` is already installed")),
        other => other,
    })?;
    require(conn, id)
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Theme>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM themes WHERE id = ?1"),
        [id],
        map,
    )
    .optional()
    .db()
}

fn require(conn: &Connection, id: &str) -> Result<Theme> {
    get(conn, id)?.ok_or_else(|| Error::NotFound(format!("theme `{id}`")))
}

pub fn list(conn: &Connection) -> Result<Vec<Theme>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM themes ORDER BY name COLLATE NOCASE, id"
        ))
        .db()?;
    let rows = stmt.query_map([], map).db()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().db()
}

pub fn active(conn: &Connection) -> Result<Option<Theme>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM themes WHERE active = 1"),
        [],
        map,
    )
    .optional()
    .db()
}

/// Makes `id` the active theme and deactivates every other one.
pub fn activate(conn: &Connection, id: &str) -> Result<Theme> {
    let tx = conn.unchecked_transaction().db()?;
    require(&tx, id)?;
    tx.execute(
        "UPDATE themes SET active = 0 WHERE active = 1 AND id <> ?1",
        [id],
    )
    .db()?;
    tx.execute("UPDATE themes SET active = 1 WHERE id = ?1", [id])
        .db()?;
    tx.commit().db()?;
    require(conn, id)
}

pub fn deactivate_all(conn: &Connection) -> Result<()> {
    conn.execute("UPDATE themes SET active = 0 WHERE active = 1", [])
        .db()?;
    Ok(())
}

pub fn set_color_scheme(conn: &Connection, id: &str, scheme: &str) -> Result<Theme> {
    if scheme.trim().is_empty() {
        return Err(Error::Invalid("color scheme must not be empty".into()));
    }
    let changed = conn
        .execute(
            "UPDATE themes SET color_scheme = ?2 WHERE id = ?1",
            params![id, scheme],
        )
        .db()?;
    if changed == 0 {
        return Err(Error::NotFound(format!("theme `{id}`")));
    }
    require(conn, id)
}

pub fn remove(conn: &Connection, id: &str) -> Result<()> {
    if conn
        .execute("DELETE FROM themes WHERE id = ?1", [id])
        .db()?
        == 0
    {
        return Err(Error::NotFound(format!("theme `{id}`")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;

    #[test]
    fn only_one_theme_is_active() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            install(c, "a/dark", "Dark", "1.0", ThemeSource::Marketplace)?;
            install(c, "b/light", "Light", "1.0", ThemeSource::Import)?;
            assert_eq!(active(c)?, None);

            activate(c, "a/dark")?;
            assert_eq!(active(c)?.unwrap().id, "a/dark");
            activate(c, "b/light")?;
            assert_eq!(active(c)?.unwrap().id, "b/light");
            assert!(!get(c, "a/dark")?.unwrap().active);
            assert_eq!(list(c)?.iter().filter(|t| t.active).count(), 1);

            deactivate_all(c)?;
            assert_eq!(active(c)?, None);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn color_scheme_and_removal() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            install(c, "a/dark", "Dark", "1.0", ThemeSource::Local)?;
            assert_eq!(
                install(c, "a/dark", "Dark", "1.0", ThemeSource::Local)
                    .unwrap_err()
                    .code(),
                "conflict"
            );
            assert_eq!(
                set_color_scheme(c, "a/dark", "ocean")?.color_scheme,
                "ocean"
            );
            assert_eq!(
                set_color_scheme(c, "a/dark", "").unwrap_err().code(),
                "invalid_input"
            );
            assert_eq!(
                set_color_scheme(c, "ghost", "x").unwrap_err().code(),
                "not_found"
            );
            remove(c, "a/dark")?;
            assert_eq!(remove(c, "a/dark").unwrap_err().code(), "not_found");
            assert_eq!(activate(c, "a/dark").unwrap_err().code(), "not_found");
            Ok(())
        })
        .unwrap();
    }
}
