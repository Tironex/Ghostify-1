//! User configuration stored as `config.toml`.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Release channel used for Ghostify's own updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    Stable,
    Beta,
}

/// How installed plugins and themes are updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoUpdate {
    /// Never update on its own.
    Off,
    /// Update automatically only if the new version requests no additional permissions.
    SafeOnly,
    /// Update everything; new permissions still require confirmation.
    All,
}

/// Light or dark appearance of the Ghostify application itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub language: String,
    pub appearance: Appearance,
    pub check_app_updates: bool,
    pub update_channel: UpdateChannel,
    pub start_with_system: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PatchConfig {
    /// Watch the Spotify installation and re-apply the patch after updates.
    pub auto_repatch: bool,
    /// Quiet period before a changed installation is inspected.
    pub repatch_debounce_ms: u64,
    /// Number of verified backups kept per installation.
    pub backups_to_keep: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginConfig {
    pub auto_update: AutoUpdate,
    /// The legacy runtime is not isolated and therefore off unless enabled here.
    pub allow_legacy_runtime: bool,
    /// Crashes inside `crash_window_secs` after which a plugin is disabled.
    pub crash_threshold: u32,
    pub crash_window_secs: u64,
    pub hook_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MarketplaceConfig {
    pub index_url: String,
    pub refresh_interval_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BridgeConfig {
    /// TCP port on 127.0.0.1; `0` picks a free port on every start.
    pub port: u16,
    pub max_message_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerformanceConfig {
    pub sample_interval_secs: u64,
    pub retention_days: u32,
}

/// Optional features that talk to other programs or services.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ExtrasConfig {
    /// Show the current track in Discord. Needs `discord_client_id`.
    pub discord_enabled: bool,
    /// Application id of a Discord application you created at discord.com/developers.
    pub discord_client_id: String,
    /// Look lyrics up on lrclib.net when they are not found on this computer.
    pub lyrics_online: bool,
}

/// Complete application configuration. Every field has a default, so a
/// partial or missing file is valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub patch: PatchConfig,
    pub plugins: PluginConfig,
    pub marketplace: MarketplaceConfig,
    pub bridge: BridgeConfig,
    pub performance: PerformanceConfig,
    pub extras: ExtrasConfig,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        GeneralConfig {
            language: "en".into(),
            appearance: Appearance::System,
            check_app_updates: true,
            update_channel: UpdateChannel::Stable,
            start_with_system: false,
        }
    }
}

impl Default for PatchConfig {
    fn default() -> Self {
        PatchConfig {
            auto_repatch: true,
            repatch_debounce_ms: 2_000,
            backups_to_keep: 3,
        }
    }
}

impl Default for PluginConfig {
    fn default() -> Self {
        PluginConfig {
            auto_update: AutoUpdate::SafeOnly,
            allow_legacy_runtime: false,
            crash_threshold: 3,
            crash_window_secs: 600,
            hook_timeout_ms: 5_000,
        }
    }
}

impl Default for MarketplaceConfig {
    fn default() -> Self {
        MarketplaceConfig {
            index_url: "https://ghostify.github.io/registry/index.json".into(),
            refresh_interval_secs: 6 * 60 * 60,
        }
    }
}

impl Default for BridgeConfig {
    fn default() -> Self {
        BridgeConfig {
            port: 0,
            max_message_bytes: 1024 * 1024,
        }
    }
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        PerformanceConfig {
            sample_interval_secs: 5,
            retention_days: 7,
        }
    }
}

impl Config {
    /// Reads the configuration. A missing file yields the defaults; an
    /// unreadable or invalid file is an error so that user settings are never
    /// silently discarded.
    pub fn load(path: &Path) -> Result<Config> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(err) => return Err(err.into()),
        };
        let config: Config =
            toml::from_str(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        config.validate()?;
        Ok(config)
    }

    /// Writes the configuration atomically (temporary file in the same
    /// directory, then rename) so that a crash never leaves a truncated file.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let text = toml::to_string_pretty(self)
            .map_err(|e| Error::Config(format!("cannot serialize configuration: {e}")))?;
        let dir = path
            .parent()
            .ok_or_else(|| Error::Config(format!("{} has no parent directory", path.display())))?;
        std::fs::create_dir_all(dir)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Rejects values that would break the application.
    pub fn validate(&self) -> Result<()> {
        if self.patch.repatch_debounce_ms < 100 {
            return Err(Error::Invalid(
                "patch.repatch_debounce_ms must be at least 100".into(),
            ));
        }
        if self.patch.backups_to_keep == 0 {
            return Err(Error::Invalid(
                "patch.backups_to_keep must be at least 1".into(),
            ));
        }
        if self.plugins.crash_threshold == 0 || self.plugins.crash_window_secs == 0 {
            return Err(Error::Invalid(
                "plugins.crash_threshold and plugins.crash_window_secs must be greater than 0"
                    .into(),
            ));
        }
        if self.plugins.hook_timeout_ms == 0 {
            return Err(Error::Invalid(
                "plugins.hook_timeout_ms must be greater than 0".into(),
            ));
        }
        if !self.marketplace.index_url.starts_with("https://") {
            return Err(Error::Invalid(
                "marketplace.index_url must use https".into(),
            ));
        }
        if self.marketplace.refresh_interval_secs < 60 {
            return Err(Error::Invalid(
                "marketplace.refresh_interval_secs must be at least 60".into(),
            ));
        }
        if self.bridge.max_message_bytes < 1024 {
            return Err(Error::Invalid(
                "bridge.max_message_bytes must be at least 1024".into(),
            ));
        }
        if self.performance.sample_interval_secs == 0 || self.performance.retention_days == 0 {
            return Err(Error::Invalid(
                "performance.sample_interval_secs and performance.retention_days must be greater than 0".into(),
            ));
        }
        let id = &self.extras.discord_client_id;
        let id_ok = id.is_empty()
            || ((15..=22).contains(&id.len()) && id.chars().all(|c| c.is_ascii_digit()));
        if !id_ok {
            return Err(Error::Invalid(
                "extras.discord_client_id must be the numeric application id (15-22 digits)".into(),
            ));
        }
        if self.extras.discord_enabled && id.is_empty() {
            return Err(Error::Invalid(
                "extras.discord_enabled needs extras.discord_client_id".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn discord_settings_are_validated() {
        let mut config = Config::default();
        config.extras.discord_enabled = true;
        assert!(config.validate().is_err(), "enabled without an id");
        for bad in ["abc", "123", "12345678901234567890123"] {
            config.extras.discord_client_id = bad.into();
            assert!(config.validate().is_err(), "{bad}");
        }
        config.extras.discord_client_id = "123456789012345678".into();
        config.validate().unwrap();
    }

    #[test]
    fn missing_file_yields_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config::load(&tmp.path().join("absent.toml")).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn partial_file_is_completed_with_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "[patch]\nbackups_to_keep = 7\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.patch.backups_to_keep, 7);
        assert!(config.patch.auto_repatch);
        assert_eq!(config.plugins, PluginConfig::default());
    }

    #[test]
    fn save_and_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("config.toml");
        let mut config = Config::default();
        config.general.appearance = Appearance::Dark;
        config.plugins.auto_update = AutoUpdate::Off;
        config.bridge.port = 49152;
        config.save(&path).unwrap();
        assert!(!path.with_extension("toml.tmp").exists());
        assert_eq!(Config::load(&path).unwrap(), config);
    }

    #[test]
    fn invalid_toml_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "this is = = not toml").unwrap();
        assert_eq!(Config::load(&path).unwrap_err().code(), "config");
    }

    #[test]
    fn invalid_values_are_rejected() {
        let mut config = Config::default();
        config.marketplace.index_url = "http://insecure.example/index.json".into();
        assert_eq!(config.validate().unwrap_err().code(), "invalid_input");

        let mut config = Config::default();
        config.plugins.crash_threshold = 0;
        assert!(config.validate().is_err());
        assert!(config
            .save(Path::new("/nonexistent-dir-for-test/config.toml"))
            .is_err());
    }
}
