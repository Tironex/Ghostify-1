//! Where the rules and assets for patching come from.

use crate::engine::Asset;
use crate::rules::{self, RuleSet};
use crate::signing::{self, SignedRuleSet, TrustedKey};
use ghostify_core::{Error, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Supplies the rule set and the files that are added to Spotify.
pub trait PatchSource: Send + Sync {
    fn load(&self) -> Result<(RuleSet, Vec<Asset>)>;
}

/// Settings the loader inside Spotify needs to find and authenticate to the bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoaderConfig {
    /// Bridge ports in the order the loader tries them.
    pub ports: Vec<u16>,
    pub secret: String,
    pub version: String,
}

impl LoaderConfig {
    pub fn new(
        ports: Vec<u16>,
        secret: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<LoaderConfig> {
        let config = LoaderConfig {
            ports,
            secret: secret.into(),
            version: version.into(),
        };
        if config.ports.is_empty() || config.ports.contains(&0) {
            return Err(Error::Invalid(
                "the loader needs at least one non-zero bridge port".into(),
            ));
        }
        if config.secret.len() < 32 || !config.secret.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(Error::Invalid(
                "the bridge secret must have at least 32 alphanumeric characters".into(),
            ));
        }
        Ok(config)
    }

    /// The script served as `ghostify/config.js`.
    pub fn to_script(&self) -> Result<String> {
        Ok(format!(
            "window.__GHOSTIFY__={};\n",
            serde_json::to_string(self)?
        ))
    }
}

/// A fixed rule set held in memory.
#[derive(Debug, Clone)]
pub struct StaticSource {
    ruleset: RuleSet,
    assets: Vec<Asset>,
}

impl StaticSource {
    pub fn new(ruleset: RuleSet, assets: Vec<Asset>) -> StaticSource {
        StaticSource { ruleset, assets }
    }

    /// The built-in rule set with the loader script, its stylesheet and the generated `config.js`.
    pub fn builtin(
        loader_js: impl Into<Vec<u8>>,
        loader_css: impl Into<Vec<u8>>,
        config: &LoaderConfig,
    ) -> Result<StaticSource> {
        Ok(StaticSource::new(
            rules::builtin(),
            vec![
                Asset {
                    path: "config.js".into(),
                    bytes: config.to_script()?.into_bytes(),
                },
                Asset {
                    path: "loader.js".into(),
                    bytes: loader_js.into(),
                },
                Asset {
                    path: "loader.css".into(),
                    bytes: loader_css.into(),
                },
            ],
        ))
    }
}

impl PatchSource for StaticSource {
    fn load(&self) -> Result<(RuleSet, Vec<Asset>)> {
        Ok((self.ruleset.clone(), self.assets.clone()))
    }
}

/// A directory with `ruleset.json` (a [`SignedRuleSet`]) and an `assets/`
/// tree. The rule set is only used if its signature verifies against one of
/// the trusted keys and is not older than `min_sequence`.
#[derive(Debug, Clone)]
pub struct DirSource {
    dir: PathBuf,
    keys: Vec<TrustedKey>,
    min_sequence: u64,
}

impl DirSource {
    pub fn new(dir: impl Into<PathBuf>, keys: Vec<TrustedKey>, min_sequence: u64) -> DirSource {
        DirSource {
            dir: dir.into(),
            keys,
            min_sequence,
        }
    }
}

impl PatchSource for DirSource {
    fn load(&self) -> Result<(RuleSet, Vec<Asset>)> {
        let text = std::fs::read(self.dir.join("ruleset.json"))?;
        let signed: SignedRuleSet = serde_json::from_slice(&text)?;
        let ruleset = signing::verify(&signed, &self.keys, self.min_sequence)?;
        let mut assets = Vec::new();
        collect_assets(&self.dir.join("assets"), Path::new(""), &mut assets)?;
        assets.sort_by(|a, b| a.path.cmp(&b.path));
        Ok((ruleset, assets))
    }
}

fn collect_assets(base: &Path, relative: &Path, out: &mut Vec<Asset>) -> Result<()> {
    let dir = base.join(relative);
    let reader = match std::fs::read_dir(&dir) {
        Ok(reader) => reader,
        Err(err)
            if err.kind() == std::io::ErrorKind::NotFound && relative.as_os_str().is_empty() =>
        {
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };
    for entry in reader {
        let entry = entry?;
        let child = relative.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_assets(base, &child, out)?;
        } else if kind.is_file() {
            let path = child
                .components()
                .map(|c| {
                    c.as_os_str().to_str().map(str::to_owned).ok_or_else(|| {
                        Error::Invalid(format!("asset name is not UTF-8: {}", child.display()))
                    })
                })
                .collect::<Result<Vec<_>>>()?
                .join("/");
            out.push(Asset {
                path,
                bytes: std::fs::read(entry.path())?,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::{encode_public_key, sign};
    use ed25519_dalek::SigningKey;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[9u8; 32])
    }

    fn trusted() -> Vec<TrustedKey> {
        vec![TrustedKey {
            key_id: "release".into(),
            public_key: encode_public_key(&key()),
        }]
    }

    fn write_dir(dir: &Path, sequence: u64) {
        let mut set = rules::builtin();
        set.version = format!("signed-{sequence}");
        set.sequence = sequence;
        let signed = sign(&set, "release", &key()).unwrap();
        std::fs::write(
            dir.join("ruleset.json"),
            serde_json::to_vec(&signed).unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("assets/fonts")).unwrap();
        std::fs::write(dir.join("assets/loader.js"), b"js").unwrap();
        std::fs::write(dir.join("assets/fonts/a.woff2"), b"font").unwrap();
    }

    #[test]
    fn static_source_returns_what_it_was_given() {
        let config = LoaderConfig::new(vec![49421, 49422], "b".repeat(64), "1.0").unwrap();
        let source = StaticSource::builtin("js", "css", &config).unwrap();
        let (set, assets) = source.load().unwrap();
        assert_eq!(set, rules::builtin());
        let paths: Vec<_> = assets.iter().map(|a| a.path.as_str()).collect();
        assert_eq!(paths, ["config.js", "loader.js", "loader.css"]);
        let script = String::from_utf8(assets[0].bytes.clone()).unwrap();
        assert!(script.starts_with("window.__GHOSTIFY__={"));
        assert!(script.contains(&"b".repeat(64)) && script.contains("[49421,49422]"));
    }

    #[test]
    fn loader_config_rejects_weak_or_empty_settings() {
        let ok = "c".repeat(32);
        assert!(LoaderConfig::new(vec![], ok.clone(), "1").is_err());
        assert!(LoaderConfig::new(vec![0], ok.clone(), "1").is_err());
        assert!(LoaderConfig::new(vec![1], "short", "1").is_err());
        assert!(LoaderConfig::new(vec![1], format!("{ok}\";alert(1)//"), "1").is_err());
        assert!(LoaderConfig::new(vec![1], ok, "1").is_ok());
    }

    #[test]
    fn dir_source_verifies_and_collects_nested_assets() {
        let tmp = tempfile::tempdir().unwrap();
        write_dir(tmp.path(), 4);
        let (set, assets) = DirSource::new(tmp.path(), trusted(), 4).load().unwrap();
        assert_eq!(set.version, "signed-4");
        let paths: Vec<_> = assets.iter().map(|a| a.path.as_str()).collect();
        assert_eq!(paths, ["fonts/a.woff2", "loader.js"]);
    }

    #[test]
    fn dir_source_rejects_untrusted_stale_and_tampered_rule_sets() {
        let tmp = tempfile::tempdir().unwrap();
        write_dir(tmp.path(), 4);
        assert_eq!(
            DirSource::new(tmp.path(), vec![], 0)
                .load()
                .unwrap_err()
                .code(),
            "permission_denied"
        );
        assert_eq!(
            DirSource::new(tmp.path(), trusted(), 5)
                .load()
                .unwrap_err()
                .code(),
            "permission_denied"
        );

        let path = tmp.path().join("ruleset.json");
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("signed-4", "signed-9");
        std::fs::write(&path, text).unwrap();
        assert_eq!(
            DirSource::new(tmp.path(), trusted(), 0)
                .load()
                .unwrap_err()
                .code(),
            "permission_denied"
        );
    }

    #[test]
    fn missing_assets_directory_means_no_assets_but_missing_rules_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        write_dir(tmp.path(), 1);
        std::fs::remove_dir_all(tmp.path().join("assets")).unwrap();
        assert!(DirSource::new(tmp.path(), trusted(), 0)
            .load()
            .unwrap()
            .1
            .is_empty());
        std::fs::remove_file(tmp.path().join("ruleset.json")).unwrap();
        assert_eq!(
            DirSource::new(tmp.path(), trusted(), 0)
                .load()
                .unwrap_err()
                .code(),
            "io"
        );
    }
}
