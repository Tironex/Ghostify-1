//! Verified backups of Spotify's original `xpui.spa`.
//!
//! Layout: `<root>/<install key>/<first 16 hex of sha256>/{xpui.spa, manifest.json}`.
//! Identical files share one backup, and every restore re-checks the hash.

use crate::fsutil::{copy_verified, sha256_file, unix_now};
use ghostify_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MANIFEST: &str = "manifest.json";
const BACKUP_FILE: &str = "xpui.spa";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifest {
    pub sha256: String,
    pub size: u64,
    pub spotify_version: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEntry {
    pub dir: PathBuf,
    pub file: PathBuf,
    pub manifest: BackupManifest,
}

#[derive(Debug, Clone)]
pub struct BackupStore {
    root: PathBuf,
}

/// Keeps install keys harmless as directory names.
fn sanitize(key: &str) -> Result<String> {
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(Error::Invalid(format!("invalid backup key `{key}`")));
    }
    Ok(key.to_owned())
}

impl BackupStore {
    pub fn new(root: impl Into<PathBuf>) -> BackupStore {
        BackupStore { root: root.into() }
    }

    fn install_dir(&self, key: &str) -> Result<PathBuf> {
        Ok(self.root.join(sanitize(key)?))
    }

    /// Stores a verified copy of `source`. If the same content is already
    /// backed up and intact, that backup is reused.
    pub fn create(
        &self,
        key: &str,
        source: &Path,
        spotify_version: Option<&str>,
    ) -> Result<BackupEntry> {
        let sha256 = sha256_file(source)?;
        if let Some(existing) = self.find_by_sha(key, &sha256)? {
            return Ok(existing);
        }
        let dir = self.install_dir(key)?.join(&sha256[..16]);
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(BACKUP_FILE);
        let copied = copy_verified(source, &file)?;
        if copied != sha256 {
            let _ = std::fs::remove_file(&file);
            return Err(Error::Conflict(
                "xpui.spa changed while the backup was being made".into(),
            ));
        }
        let manifest = BackupManifest {
            size: std::fs::metadata(&file)?.len(),
            sha256,
            spotify_version: spotify_version.map(str::to_owned),
            created_at: unix_now(),
        };
        // The manifest is written last, so a backup without one is incomplete and ignored.
        std::fs::write(dir.join(MANIFEST), serde_json::to_vec_pretty(&manifest)?)?;
        Ok(BackupEntry {
            dir,
            file,
            manifest,
        })
    }

    fn read_entry(dir: &Path) -> Option<BackupEntry> {
        let manifest: BackupManifest =
            serde_json::from_slice(&std::fs::read(dir.join(MANIFEST)).ok()?).ok()?;
        let file = dir.join(BACKUP_FILE);
        file.is_file().then(|| BackupEntry {
            dir: dir.to_path_buf(),
            file,
            manifest,
        })
    }

    /// Complete backups of an installation, newest first.
    pub fn list(&self, key: &str) -> Result<Vec<BackupEntry>> {
        let dir = self.install_dir(key)?;
        let reader = match std::fs::read_dir(&dir) {
            Ok(reader) => reader,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };
        let mut entries: Vec<BackupEntry> = reader
            .filter_map(|e| e.ok())
            .filter_map(|e| Self::read_entry(&e.path()))
            .collect();
        entries.sort_by(|a, b| {
            b.manifest
                .created_at
                .cmp(&a.manifest.created_at)
                .then_with(|| b.dir.cmp(&a.dir))
        });
        Ok(entries)
    }

    /// The intact backup of the file with this hash, if any.
    pub fn find_by_sha(&self, key: &str, sha256: &str) -> Result<Option<BackupEntry>> {
        if sha256.len() < 16 {
            return Ok(None);
        }
        let dir = self.install_dir(key)?.join(&sha256[..16]);
        let Some(entry) = Self::read_entry(&dir) else {
            return Ok(None);
        };
        if entry.manifest.sha256 != sha256 {
            return Ok(None);
        }
        // A damaged backup is worse than none, so it is not offered.
        match sha256_file(&entry.file) {
            Ok(actual) if actual == sha256 => Ok(Some(entry)),
            _ => Ok(None),
        }
    }

    /// Puts the backup back at `target`, verifying it before and after.
    pub fn restore(&self, entry: &BackupEntry, target: &Path) -> Result<()> {
        let copied = copy_verified(&entry.file, target)?;
        if copied != entry.manifest.sha256 {
            return Err(Error::Conflict(format!(
                "backup {} is corrupted; reinstall Spotify to repair",
                entry.dir.display()
            )));
        }
        Ok(())
    }

    /// Keeps the newest `keep` backups plus the one with hash `protect`.
    /// Returns how many were deleted.
    pub fn prune(&self, key: &str, keep: usize, protect: Option<&str>) -> Result<usize> {
        let mut removed = 0;
        for (index, entry) in self.list(key)?.into_iter().enumerate() {
            if index < keep || protect == Some(entry.manifest.sha256.as_str()) {
                continue;
            }
            std::fs::remove_dir_all(&entry.dir)?;
            removed += 1;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::sha256_bytes;

    fn setup() -> (tempfile::TempDir, BackupStore, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let store = BackupStore::new(tmp.path().join("backups"));
        let spa = tmp.path().join("xpui.spa");
        (tmp, store, spa)
    }

    #[test]
    fn create_list_and_restore() {
        let (_tmp, store, spa) = setup();
        std::fs::write(&spa, b"original").unwrap();
        let entry = store.create("install-1", &spa, Some("1.2.3")).unwrap();
        assert_eq!(entry.manifest.sha256, sha256_bytes(b"original"));
        assert_eq!(entry.manifest.spotify_version.as_deref(), Some("1.2.3"));
        assert_eq!(entry.manifest.size, 8);

        std::fs::write(&spa, b"patched content").unwrap();
        store.restore(&entry, &spa).unwrap();
        assert_eq!(std::fs::read(&spa).unwrap(), b"original");
        assert_eq!(store.list("install-1").unwrap(), [entry]);
        assert!(store.list("install-2").unwrap().is_empty());
    }

    #[test]
    fn identical_content_is_stored_once() {
        let (_tmp, store, spa) = setup();
        std::fs::write(&spa, b"same").unwrap();
        let first = store.create("k", &spa, None).unwrap();
        let second = store.create("k", &spa, Some("later")).unwrap();
        assert_eq!(first, second);
        assert_eq!(store.list("k").unwrap().len(), 1);
    }

    #[test]
    fn corrupted_backups_are_not_offered_and_not_restored() {
        let (_tmp, store, spa) = setup();
        std::fs::write(&spa, b"original").unwrap();
        let entry = store.create("k", &spa, None).unwrap();
        std::fs::write(&entry.file, b"tampered").unwrap();
        assert_eq!(
            store.find_by_sha("k", &entry.manifest.sha256).unwrap(),
            None
        );

        std::fs::write(&spa, b"current").unwrap();
        let err = store.restore(&entry, &spa).unwrap_err();
        assert_eq!(err.code(), "conflict");
    }

    #[test]
    fn incomplete_backups_are_ignored() {
        let (_tmp, store, spa) = setup();
        std::fs::write(&spa, b"original").unwrap();
        let entry = store.create("k", &spa, None).unwrap();
        std::fs::remove_file(entry.dir.join(MANIFEST)).unwrap();
        assert!(store.list("k").unwrap().is_empty());
        assert_eq!(
            store.find_by_sha("k", &entry.manifest.sha256).unwrap(),
            None
        );
        // Creating again repairs it.
        let again = store.create("k", &spa, None).unwrap();
        assert_eq!(store.list("k").unwrap(), [again]);
    }

    #[test]
    fn prune_keeps_newest_and_protected() {
        let (_tmp, store, spa) = setup();
        let mut shas = Vec::new();
        for i in 0..4 {
            std::fs::write(&spa, format!("version {i}")).unwrap();
            let entry = store.create("k", &spa, None).unwrap();
            // Distinct timestamps make the order deterministic.
            let mut manifest = entry.manifest.clone();
            manifest.created_at = 1_000 + i;
            std::fs::write(
                entry.dir.join(MANIFEST),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            shas.push(manifest.sha256);
        }
        let removed = store.prune("k", 1, Some(&shas[0])).unwrap();
        assert_eq!(removed, 2);
        let left: Vec<_> = store
            .list("k")
            .unwrap()
            .into_iter()
            .map(|e| e.manifest.sha256)
            .collect();
        assert_eq!(left, [shas[3].clone(), shas[0].clone()]);
    }

    #[test]
    fn hostile_keys_are_rejected() {
        let (_tmp, store, spa) = setup();
        std::fs::write(&spa, b"x").unwrap();
        for key in ["", "../escape", "a/b", "a b"] {
            assert_eq!(
                store.create(key, &spa, None).unwrap_err().code(),
                "invalid_input",
                "{key}"
            );
        }
    }
}
