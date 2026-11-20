//! The shared secret between Ghostify and the loader in one Spotify installation.

use ghostify_core::{Error, Result};
use ghostify_db::{repo::settings, Db};

const SETTING_KEY: &str = "bridge.secret";

/// 256 random bits as hex.
pub fn generate() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| Error::Internal(format!("no secure random source available: {e}")))?;
    let mut text = String::with_capacity(64);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    Ok(text)
}

/// Returns the stored secret, creating and storing one on first use.
pub fn load_or_create(db: &Db) -> Result<String> {
    db.with(|conn| {
        if let Some(existing) = settings::get::<String>(conn, SETTING_KEY)? {
            if existing.len() == 64 {
                return Ok(existing);
            }
        }
        let secret = generate()?;
        settings::set(conn, SETTING_KEY, &secret)?;
        Ok(secret)
    })
}

/// Replaces the secret. Patched installations must be patched again, which
/// invalidates every loader that still knows the old one.
pub fn rotate(db: &Db) -> Result<String> {
    let secret = generate()?;
    db.with(|conn| settings::set(conn, SETTING_KEY, &secret))?;
    Ok(secret)
}

/// Comparison whose duration does not depend on where the inputs differ.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_secrets_are_long_hex_and_unique() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn secret_is_created_once_and_can_be_rotated() {
        let db = Db::open_in_memory().unwrap();
        let first = load_or_create(&db).unwrap();
        assert_eq!(load_or_create(&db).unwrap(), first);
        let rotated = rotate(&db).unwrap();
        assert_ne!(rotated, first);
        assert_eq!(load_or_create(&db).unwrap(), rotated);
    }

    #[test]
    fn corrupt_stored_secret_is_replaced() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| settings::set(c, SETTING_KEY, &"short"))
            .unwrap();
        assert_eq!(load_or_create(&db).unwrap().len(), 64);
    }

    #[test]
    fn constant_time_comparison_behaves_like_equality() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(!constant_time_eq("", "a"));
        assert!(constant_time_eq("", ""));
    }
}
