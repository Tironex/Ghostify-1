//! Structure and validation of the marketplace index.

use ghostify_core::{Error, Result};
use ghostify_db::repo::market::{Listing, ListingKind, Package};
use ghostify_plugins::manifest::{validate_id, Permission};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const MAX_LISTINGS: usize = 20_000;
pub const MAX_PACKAGE_BYTES: u64 = 20 * 1024 * 1024;

/// Hosts packages may be downloaded from. Release assets on GitHub redirect to
/// `objects.githubusercontent.com`.
pub const ALLOWED_DOWNLOAD_HOSTS: &[&str] = &[
    "github.com",
    "raw.githubusercontent.com",
    "objects.githubusercontent.com",
    "codeload.github.com",
    "release-assets.githubusercontent.com",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub id: String,
    pub kind: ListingKind,
    pub name: String,
    pub author: String,
    /// `owner/repo` on GitHub.
    pub repo: String,
    pub category: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub latest_version: String,
    #[serde(default)]
    pub stars: i64,
    #[serde(default)]
    pub rating_avg: Option<f64>,
    #[serde(default)]
    pub rating_count: i64,
    pub download_url: String,
    /// SHA-256 of the zip, lowercase hex.
    pub sha256: String,
    pub size: u64,
    /// Permissions of the package (plugins). Checked against the real manifest on install.
    #[serde(default)]
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Index {
    /// Increases with every published index; protects against replay.
    pub sequence: u64,
    pub generated_at: i64,
    pub listings: Vec<IndexEntry>,
}

impl IndexEntry {
    pub fn listing(&self) -> Listing {
        Listing {
            id: self.id.clone(),
            kind: self.kind,
            name: self.name.clone(),
            author: self.author.clone(),
            repo: self.repo.clone(),
            category: self.category.clone(),
            description: self.description.clone(),
            tags: self.tags.clone(),
            latest_version: self.latest_version.clone(),
            stars: self.stars,
            rating_avg: self.rating_avg,
            rating_count: self.rating_count,
        }
    }

    pub fn package(&self) -> Package {
        Package {
            listing_id: self.id.clone(),
            version: self.latest_version.clone(),
            download_url: self.download_url.clone(),
            sha256: self.sha256.clone(),
            size: self.size,
            permissions: self.permissions.clone(),
        }
    }

    fn validate(&self) -> Result<()> {
        let id = &self.id;
        validate_id(id)?;
        let text = |label: &str, value: &str, min: usize, max: usize| -> Result<()> {
            if value.len() < min || value.len() > max || value.chars().any(char::is_control) {
                return Err(Error::Invalid(format!("listing `{id}`: {label} must have {min}-{max} characters without control characters")));
            }
            Ok(())
        };
        text("name", &self.name, 1, 80)?;
        text("author", &self.author, 0, 80)?;
        text("category", &self.category, 1, 40)?;
        text("description", &self.description, 0, 500)?;
        text("version", &self.latest_version, 1, 32)?;
        if self.tags.len() > 10 {
            return Err(Error::Invalid(format!("listing `{id}`: at most 10 tags")));
        }
        for tag in &self.tags {
            text("tag", tag, 1, 30)?;
        }
        let repo_ok = self.repo.split_once('/').is_some_and(|(owner, name)| {
            let part = |p: &str| {
                !p.is_empty()
                    && p.len() <= 100
                    && p.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            };
            part(owner) && part(name)
        });
        if !repo_ok {
            return Err(Error::Invalid(format!(
                "listing `{id}`: `{}` is not an `owner/repo` name",
                self.repo
            )));
        }
        if self.stars < 0
            || self.rating_count < 0
            || self.rating_avg.is_some_and(|r| !(0.0..=5.0).contains(&r))
        {
            return Err(Error::Invalid(format!(
                "listing `{id}`: stars and ratings are out of range"
            )));
        }
        if self.size == 0 || self.size > MAX_PACKAGE_BYTES {
            return Err(Error::Invalid(format!(
                "listing `{id}`: the package size must be 1..={MAX_PACKAGE_BYTES} bytes"
            )));
        }
        if self.sha256.len() != 64
            || !self
                .sha256
                .chars()
                .all(|c| matches!(c, '0'..='9' | 'a'..='f'))
        {
            return Err(Error::Invalid(format!(
                "listing `{id}`: sha256 must be 64 lowercase hex digits"
            )));
        }
        check_download_url(&self.download_url)
            .map_err(|e| Error::Invalid(format!("listing `{id}`: {e}")))?;
        for permission in &self.permissions {
            Permission::parse(permission)?;
        }
        if self.kind == ListingKind::Theme && !self.permissions.is_empty() {
            return Err(Error::Invalid(format!(
                "listing `{id}`: themes have no permissions"
            )));
        }
        Ok(())
    }
}

impl Index {
    pub fn validate(&self) -> Result<()> {
        if self.listings.len() > MAX_LISTINGS {
            return Err(Error::Invalid(format!(
                "an index may have at most {MAX_LISTINGS} listings"
            )));
        }
        let mut seen = HashSet::new();
        for entry in &self.listings {
            entry.validate()?;
            if !seen.insert(&entry.id) {
                return Err(Error::Invalid(format!(
                    "listing `{}` appears twice",
                    entry.id
                )));
            }
        }
        Ok(())
    }
}

/// Download addresses are https URLs on a GitHub host, without credentials or ports.
pub fn check_download_url(url: &str) -> Result<()> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| Error::Invalid("the download url must use https".into()))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty()
        || authority.contains(['@', ':'])
        || url.len() > 2048
        || url.chars().any(|c| c.is_control() || c == ' ')
    {
        return Err(Error::Invalid("the download url is malformed".into()));
    }
    if !ALLOWED_DOWNLOAD_HOSTS.contains(&authority.to_ascii_lowercase().as_str()) {
        return Err(Error::Invalid(format!(
            "downloads from `{authority}` are not allowed"
        )));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn entry(id: &str, kind: ListingKind, version: &str) -> IndexEntry {
        IndexEntry {
            id: id.into(),
            kind,
            name: format!("Name {id}"),
            author: "author".into(),
            repo: "owner/repo".into(),
            category: "tools".into(),
            description: "does things".into(),
            tags: vec!["a".into()],
            latest_version: version.into(),
            stars: 10,
            rating_avg: Some(4.5),
            rating_count: 3,
            download_url: format!(
                "https://github.com/owner/repo/releases/download/v{version}/{}.zip",
                id.replace('/', "-")
            ),
            sha256: "ab".repeat(32),
            size: 1000,
            permissions: vec![],
        }
    }

    pub fn sample_index(sequence: u64) -> Index {
        Index {
            sequence,
            generated_at: 1_700_000_000,
            listings: vec![
                entry("me/plug", ListingKind::Plugin, "1.0.0"),
                entry("me/theme", ListingKind::Theme, "2.0.0"),
            ],
        }
    }

    fn invalid(edit: impl FnOnce(&mut IndexEntry)) -> bool {
        let mut index = sample_index(1);
        edit(&mut index.listings[0]);
        index.validate().is_err()
    }

    #[test]
    fn a_valid_index_passes_and_converts() {
        let index = sample_index(1);
        index.validate().unwrap();
        let entry = &index.listings[0];
        assert_eq!(entry.listing().latest_version, "1.0.0");
        assert_eq!(entry.package().sha256, "ab".repeat(32));
    }

    #[test]
    fn download_urls_are_restricted_to_github_https() {
        for ok in [
            "https://github.com/a/b/releases/download/v1/x.zip",
            "https://raw.githubusercontent.com/a/b/main/x.zip",
            "https://GitHub.com/a/b.zip",
        ] {
            assert!(check_download_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://github.com/a.zip",
            "https://evil.example/a.zip",
            "https://github.com.evil.example/a.zip",
            "https://user@github.com/a.zip",
            "https://github.com:8443/a.zip",
            "https://evil.example\\@github.com/a.zip",
            "https://",
            "ftp://github.com/a.zip",
            "https://github.com/a b.zip",
            "//github.com/a.zip",
        ] {
            assert!(check_download_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn invalid_entries_are_rejected() {
        assert!(invalid(|e| e.id = "Bad Id".into()));
        assert!(invalid(|e| e.name.clear()));
        assert!(invalid(|e| e.name = "x".repeat(81)));
        assert!(invalid(|e| e.description = "x\u{7}".into()));
        assert!(invalid(|e| e.tags = vec!["t".into(); 11]));
        assert!(invalid(|e| e.repo = "no-slash".into()));
        assert!(invalid(|e| e.repo = "a/b/c".into()));
        assert!(invalid(|e| e.repo = "a b/c".into()));
        assert!(invalid(|e| e.stars = -1));
        assert!(invalid(|e| e.rating_avg = Some(5.1)));
        assert!(invalid(|e| e.size = 0));
        assert!(invalid(|e| e.size = MAX_PACKAGE_BYTES + 1));
        assert!(invalid(|e| e.sha256 = "AB".repeat(32)));
        assert!(invalid(|e| e.sha256 = "ab".repeat(31)));
        assert!(invalid(
            |e| e.download_url = "https://evil.example/x.zip".into()
        ));
        assert!(invalid(|e| e.permissions = vec!["root".into()]));
        assert!(invalid(|e| {
            e.kind = ListingKind::Theme;
            e.permissions = vec!["storage".into()];
        }));
    }

    #[test]
    fn duplicates_and_oversized_indexes_are_rejected() {
        let mut index = sample_index(1);
        index.listings.push(index.listings[0].clone());
        assert!(index.validate().is_err());
        let mut big = sample_index(1);
        big.listings = (0..=MAX_LISTINGS)
            .map(|i| entry(&format!("p{i}"), ListingKind::Plugin, "1"))
            .collect();
        assert!(big.validate().is_err());
    }

    #[test]
    fn unknown_fields_in_the_json_are_ignored_but_required_ones_are_not() {
        let json = r#"{"sequence":1,"generated_at":0,"listings":[],"future_field":true}"#;
        assert!(serde_json::from_str::<Index>(json).is_ok());
        assert!(serde_json::from_str::<Index>(r#"{"listings":[]}"#).is_err());
    }
}
