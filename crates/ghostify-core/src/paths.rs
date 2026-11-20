//! Platform specific locations of Ghostify's files.

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};

/// Environment variable that switches Ghostify into portable mode: all data,
/// configuration and cache live below the given directory.
pub const HOME_ENV: &str = "GHOSTIFY_HOME";

const APP_DIR: &str = "ghostify";

/// All directories Ghostify reads from and writes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    data_dir: PathBuf,
    config_dir: PathBuf,
    cache_dir: PathBuf,
}

impl AppPaths {
    /// Resolves the locations for the current user. `GHOSTIFY_HOME` takes
    /// precedence over the platform defaults.
    pub fn discover() -> Result<Self> {
        if let Some(root) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self::from_root(PathBuf::from(root)));
        }
        let data = dirs::data_dir()
            .ok_or_else(|| Error::Config("cannot determine the user data directory".into()))?;
        let config = dirs::config_dir()
            .ok_or_else(|| Error::Config("cannot determine the user config directory".into()))?;
        let cache = dirs::cache_dir()
            .ok_or_else(|| Error::Config("cannot determine the user cache directory".into()))?;
        Ok(AppPaths {
            data_dir: data.join(APP_DIR),
            config_dir: config.join(APP_DIR),
            cache_dir: cache.join(APP_DIR),
        })
    }

    /// Places everything below `root` (`data/`, `config/`, `cache/`).
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        AppPaths {
            data_dir: root.join("data"),
            config_dir: root.join("config"),
            cache_dir: root.join("cache"),
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn database_file(&self) -> PathBuf {
        self.data_dir.join("ghostify.db")
    }

    pub fn plugins_dir(&self) -> PathBuf {
        self.data_dir.join("plugins")
    }

    pub fn themes_dir(&self) -> PathBuf {
        self.data_dir.join("themes")
    }

    /// Verified copies of Spotify's original files, written before patching.
    pub fn backups_dir(&self) -> PathBuf {
        self.data_dir.join("backups")
    }

    /// Previous plugin versions kept for rollback.
    pub fn snapshots_dir(&self) -> PathBuf {
        self.data_dir.join("snapshots")
    }

    /// Downloads are unpacked and verified here before they are moved into place.
    pub fn staging_dir(&self) -> PathBuf {
        self.cache_dir.join("staging")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// Creates every directory. Safe to call repeatedly.
    pub fn ensure(&self) -> Result<()> {
        for dir in [
            self.data_dir.clone(),
            self.config_dir.clone(),
            self.cache_dir.clone(),
            self.plugins_dir(),
            self.themes_dir(),
            self.backups_dir(),
            self.snapshots_dir(),
            self.staging_dir(),
            self.logs_dir(),
        ] {
            std::fs::create_dir_all(&dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_layout() {
        let paths = AppPaths::from_root("/tmp/gh");
        assert_eq!(
            paths.database_file(),
            PathBuf::from("/tmp/gh/data/ghostify.db")
        );
        assert_eq!(
            paths.config_file(),
            PathBuf::from("/tmp/gh/config/config.toml")
        );
        assert_eq!(paths.staging_dir(), PathBuf::from("/tmp/gh/cache/staging"));
    }

    #[test]
    fn ensure_creates_all_directories_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(tmp.path());
        paths.ensure().unwrap();
        paths.ensure().unwrap();
        for dir in [
            paths.plugins_dir(),
            paths.themes_dir(),
            paths.backups_dir(),
            paths.snapshots_dir(),
            paths.staging_dir(),
            paths.logs_dir(),
        ] {
            assert!(dir.is_dir(), "{} missing", dir.display());
        }
    }
}
