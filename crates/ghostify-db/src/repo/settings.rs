//! Key-value settings stored as JSON.

use super::now;
use crate::db::SqlExt;
use ghostify_core::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};

pub fn get<T: DeserializeOwned>(conn: &Connection, key: &str) -> Result<Option<T>> {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()
        .db()?;
    match raw {
        Some(text) => Ok(Some(serde_json::from_str(&text)?)),
        None => Ok(None),
    }
}

pub fn set<T: Serialize>(conn: &Connection, key: &str, value: &T) -> Result<()> {
    let json = serde_json::to_string(value)?;
    conn.execute(
        "INSERT INTO settings(key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, json, now()],
    )
    .db()?;
    Ok(())
}

/// Returns `true` if the key existed.
pub fn delete(conn: &Connection, key: &str) -> Result<bool> {
    Ok(conn
        .execute("DELETE FROM settings WHERE key = ?1", [key])
        .db()?
        > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Sample {
        name: String,
        values: Vec<u8>,
    }

    #[test]
    fn set_get_overwrite_delete() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            assert_eq!(get::<i64>(c, "missing")?, None);
            let sample = Sample {
                name: "a".into(),
                values: vec![1, 2],
            };
            set(c, "sample", &sample)?;
            assert_eq!(get::<Sample>(c, "sample")?, Some(sample));
            set(
                c,
                "sample",
                &Sample {
                    name: "b".into(),
                    values: vec![],
                },
            )?;
            assert_eq!(get::<Sample>(c, "sample")?.unwrap().name, "b");
            assert!(delete(c, "sample")?);
            assert!(!delete(c, "sample")?);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn type_mismatch_is_a_serialization_error() {
        let db = Db::open_in_memory().unwrap();
        let err = db
            .with(|c| {
                set(c, "n", &"text")?;
                get::<i64>(c, "n")
            })
            .unwrap_err();
        assert_eq!(err.code(), "serialization");
    }
}
