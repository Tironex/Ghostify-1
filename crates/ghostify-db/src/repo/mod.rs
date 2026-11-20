//! Repositories: typed, synchronous access to each table group. Every
//! function takes a `&Connection`, so callers compose them freely inside
//! [`crate::Db::with`] / [`crate::Db::call`] and inside transactions.

/// Declares an enum that is stored as a fixed set of strings in the database.
macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        pub enum $name {
            $(#[serde(rename = $text)] $variant),+
        }

        impl $name {
            /// The value stored in the database.
            pub fn as_str(&self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            /// Parses a stored value.
            pub fn parse(value: &str) -> ghostify_core::Result<Self> {
                match value {
                    $($text => Ok(Self::$variant),)+
                    other => Err(ghostify_core::Error::Invalid(
                        format!("unknown {} `{}`", stringify!($name), other),
                    )),
                }
            }
        }
    };
}

pub mod installs;
pub mod market;
pub mod misc;
pub mod patch;
pub mod plugins;
pub mod settings;
pub mod themes;

use ghostify_core::Result;
use std::time::{SystemTime, UNIX_EPOCH};

/// Current time in Unix seconds.
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Lifts a domain error raised while mapping a row into a rusqlite error.
pub(crate) fn col<T>(result: Result<T>) -> rusqlite::Result<T> {
    result.map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}
