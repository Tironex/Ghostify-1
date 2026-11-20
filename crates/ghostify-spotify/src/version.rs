//! Spotify version numbers such as `1.2.31.1205.g4d59ad7c`.

use ghostify_core::{Error, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cmp::Ordering;
use std::fmt;

/// A Spotify client version. Only the leading numeric components take part in
/// comparisons; a trailing build hash (`g4d59ad7c`) is kept for display only.
#[derive(Debug, Clone)]
pub struct SpotifyVersion {
    raw: String,
    parts: Vec<u32>,
}

impl SpotifyVersion {
    /// Parses a version with at least two numeric components.
    pub fn parse(text: &str) -> Result<SpotifyVersion> {
        let raw = text.trim();
        let parts: Vec<u32> = raw
            .split('.')
            .map_while(|segment| segment.parse::<u32>().ok())
            .collect();
        if parts.len() < 2 {
            return Err(Error::Invalid(format!("`{raw}` is not a Spotify version")));
        }
        Ok(SpotifyVersion {
            raw: raw.to_owned(),
            parts,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn parts(&self) -> &[u32] {
        &self.parts
    }
}

impl PartialEq for SpotifyVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for SpotifyVersion {}

impl PartialOrd for SpotifyVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SpotifyVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        let len = self.parts.len().max(other.parts.len());
        (0..len)
            .map(|i| {
                let a = self.parts.get(i).copied().unwrap_or(0);
                let b = other.parts.get(i).copied().unwrap_or(0);
                a.cmp(&b)
            })
            .find(|ordering| *ordering != Ordering::Equal)
            .unwrap_or(Ordering::Equal)
    }
}

impl fmt::Display for SpotifyVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl Serialize for SpotifyVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for SpotifyVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        SpotifyVersion::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Inclusive version interval. A missing bound is open.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VersionRange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<SpotifyVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<SpotifyVersion>,
}

impl VersionRange {
    pub fn is_unbounded(&self) -> bool {
        self.min.is_none() && self.max.is_none()
    }

    pub fn contains(&self, version: &SpotifyVersion) -> bool {
        self.min.as_ref().is_none_or(|min| version >= min)
            && self.max.as_ref().is_none_or(|max| version <= max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> SpotifyVersion {
        SpotifyVersion::parse(text).unwrap()
    }

    #[test]
    fn parses_numeric_prefix_and_ignores_build_hash() {
        let version = v("1.2.31.1205.g4d59ad7c");
        assert_eq!(version.parts(), [1, 2, 31, 1205]);
        assert_eq!(version.as_str(), "1.2.31.1205.g4d59ad7c");
        assert_eq!(v(" 1.2 ").parts(), [1, 2]);
    }

    #[test]
    fn rejects_non_versions() {
        for bad in ["", "1", "abc", "x.1.2", ".1.2"] {
            assert_eq!(
                SpotifyVersion::parse(bad).unwrap_err().code(),
                "invalid_input",
                "{bad}"
            );
        }
    }

    #[test]
    fn compares_numerically_not_lexically() {
        assert!(v("1.2.100") > v("1.2.31"));
        assert!(v("1.10") > v("1.9.5"));
        assert_eq!(v("1.2"), v("1.2.0.0"));
        assert_eq!(v("1.2.3.4.gaaa"), v("1.2.3.4.gbbb"));
        assert!(v("1.2.31.1205") < v("1.2.31.1206"));
    }

    #[test]
    fn range_bounds_are_inclusive_and_may_be_open() {
        let range = VersionRange {
            min: Some(v("1.2.30")),
            max: Some(v("1.2.40")),
        };
        assert!(range.contains(&v("1.2.30")));
        assert!(!range.contains(&v("1.2.40.9")));
        assert!(range.contains(&v("1.2.35.7")));
        assert!(!range.contains(&v("1.2.29.9")));
        assert!(VersionRange::default().contains(&v("9.9")));
        assert!(VersionRange::default().is_unbounded());
        let open_top = VersionRange {
            min: Some(v("1.2")),
            max: None,
        };
        assert!(open_top.contains(&v("5.0")));
    }

    #[test]
    fn serde_round_trip_uses_plain_strings() {
        let range = VersionRange {
            min: Some(v("1.2.30.1")),
            max: None,
        };
        let json = serde_json::to_string(&range).unwrap();
        assert_eq!(json, r#"{"min":"1.2.30.1"}"#);
        assert_eq!(serde_json::from_str::<VersionRange>(&json).unwrap(), range);
        assert!(serde_json::from_str::<VersionRange>(r#"{"max":"nope"}"#).is_err());
        assert!(serde_json::from_str::<VersionRange>("{}")
            .unwrap()
            .is_unbounded());
    }
}
