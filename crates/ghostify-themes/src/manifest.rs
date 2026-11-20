//! `theme.json`: the manifest of a Ghostify theme.

use ghostify_core::{Error, Result};
use serde::{Deserialize, Serialize};

pub const MANIFEST_FILE: &str = "theme.json";
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// Which UI mode a theme is designed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Dark,
    Light,
    /// Works in both modes.
    #[default]
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeManifest {
    /// `name` or `author/name`; lowercase letters, digits, `.`, `_`, `-`.
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    /// Stylesheet relative to the theme folder.
    #[serde(default = "default_css")]
    pub css: String,
    /// Optional Spicetify style colour file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_ini: Option<String>,
    /// Colour scheme used when none is chosen; defaults to the first one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_scheme: Option<String>,
    #[serde(default)]
    pub mode: Mode,
}

fn default_css() -> String {
    "user.css".into()
}

impl ThemeManifest {
    pub fn parse(bytes: &[u8]) -> Result<ThemeManifest> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(Error::Invalid("theme.json is too large".into()));
        }
        let manifest: ThemeManifest = serde_json::from_slice(bytes)
            .map_err(|e| Error::Invalid(format!("theme.json is invalid: {e}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn to_json(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec_pretty(self)?)
    }

    pub fn validate(&self) -> Result<()> {
        validate_id(&self.id)?;
        for (label, value, max) in [
            ("name", &self.name, 80),
            ("version", &self.version, 32),
            ("author", &self.author, 80),
            ("description", &self.description, 500),
        ] {
            if value.len() > max || value.chars().any(char::is_control) {
                return Err(Error::Invalid(format!(
                    "theme {label} is too long or contains control characters"
                )));
            }
        }
        if self.name.trim().is_empty() || self.version.trim().is_empty() {
            return Err(Error::Invalid(
                "theme name and version must not be empty".into(),
            ));
        }
        validate_relative(&self.css, "css")?;
        if let Some(ini) = &self.color_ini {
            validate_relative(ini, "color_ini")?;
        }
        Ok(())
    }

    /// Folder name below the themes directory.
    pub fn dir_name(&self) -> String {
        dir_name(&self.id)
    }
}

pub fn dir_name(id: &str) -> String {
    id.replace('/', "__")
}

pub fn validate_id(id: &str) -> Result<()> {
    let part_ok = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && part.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
            })
            && part != ".."
    };
    let parts: Vec<&str> = id.split('/').collect();
    if id.len() > 64 || parts.len() > 2 || !parts.iter().all(|p| part_ok(p)) {
        return Err(Error::Invalid(format!(
            "`{id}` is not a valid theme id (use `name` or `author/name`, lowercase letters, digits, . _ -)"
        )));
    }
    Ok(())
}

/// A relative path with `/` separators and no `..`, empty or hidden components.
pub fn validate_relative(path: &str, label: &str) -> Result<()> {
    let bad = path.is_empty()
        || path.len() > 200
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == ".." || c.starts_with('.'));
    if bad {
        return Err(Error::Invalid(format!(
            "{label}: `{path}` is not a safe relative path"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(extra: &str) -> String {
        format!(r#"{{"id":"me/dark","name":"Dark","version":"1.0.0"{extra}}}"#)
    }

    #[test]
    fn minimal_manifest_gets_defaults() {
        let m = ThemeManifest::parse(json("").as_bytes()).unwrap();
        assert_eq!(m.css, "user.css");
        assert_eq!(m.mode, Mode::Any);
        assert_eq!(m.dir_name(), "me__dark");
        assert!(m.color_ini.is_none());
    }

    #[test]
    fn round_trips() {
        let m = ThemeManifest::parse(
            json(r#","author":"me","mode":"dark","color_ini":"color.ini","default_scheme":"Base""#)
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(ThemeManifest::parse(&m.to_json().unwrap()).unwrap(), m);
        assert_eq!(m.mode, Mode::Dark);
    }

    #[test]
    fn rejects_bad_ids() {
        for id in [
            "",
            "Upper",
            "a/b/c",
            "../x",
            "a/..",
            "a b",
            "/a",
            "a/",
            ".hidden",
            &"x".repeat(65),
            "a\\b",
        ] {
            assert!(validate_id(id).is_err(), "{id:?}");
        }
        for id in ["x", "me/dark-2", "a.b_c/d"] {
            assert!(validate_id(id).is_ok(), "{id:?}");
        }
    }

    #[test]
    fn rejects_unsafe_paths_and_fields() {
        for path in [
            "",
            "/etc/passwd",
            "../x.css",
            "a/../b.css",
            "a\\b.css",
            "C:/x.css",
            ".hidden.css",
            "a//b.css",
            "./a.css",
        ] {
            assert!(validate_relative(path, "css").is_err(), "{path:?}");
        }
        assert!(ThemeManifest::parse(json(r#","css":"../x.css""#).as_bytes()).is_err());
        assert!(ThemeManifest::parse(br#"{"id":"a","name":" ","version":"1"}"#).is_err());
        assert!(ThemeManifest::parse(br#"{"id":"a","name":"n"}"#).is_err());
        assert!(ThemeManifest::parse(b"not json").is_err());
        assert!(ThemeManifest::parse(&vec![b' '; MAX_MANIFEST_BYTES + 1]).is_err());
        assert!(ThemeManifest::parse(json(r#","mode":"purple""#).as_bytes()).is_err());
    }
}
