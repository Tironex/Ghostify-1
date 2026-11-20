//! Applying a rule set to `xpui.spa`.
//!
//! The archive is rewritten into a temporary file next to the original.
//! Entries that no rule touches are copied in their compressed form (no
//! decompression, so memory use and time stay low), the result is verified,
//! and only then renamed over the original. A failure at any point leaves the
//! original file untouched.

use crate::fsutil::{sha256_file, temp_sibling, unix_now, zip_err};
use crate::rules::{validate_entry_name, RuleSet};
use ghostify_core::{Error, Result};
use ghostify_spotify::SpotifyVersion;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Entry that marks a patched bundle and records what was applied.
pub const PATCH_INFO_ENTRY: &str = "ghostify/patched.json";
/// All files Ghostify adds live below this prefix.
pub const ASSET_PREFIX: &str = "ghostify/";

/// A file Ghostify adds to the bundle, for example the loader script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// Path below `ghostify/`, for example `loader.js`.
    pub path: String,
    pub bytes: Vec<u8>,
}

/// What a patched bundle records about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchInfo {
    pub ruleset_version: String,
    pub ruleset_sequence: u64,
    pub spotify_version: Option<String>,
    /// SHA-256 of the unpatched bundle; identifies the backup to restore.
    pub original_sha256: String,
    pub applied_rules: Vec<String>,
    pub applied_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyReport {
    pub info: PatchInfo,
    /// Optional rules that found nothing to change.
    pub skipped_rules: Vec<String>,
    pub patched_sha256: String,
}

fn open_archive(path: &Path) -> Result<ZipArchive<File>> {
    ZipArchive::new(File::open(path)?).map_err(zip_err)
}

fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<Vec<u8>> {
    let mut entry = archive.by_name(name).map_err(|e| match e {
        zip::result::ZipError::FileNotFound => Error::NotFound(format!("`{name}` in xpui.spa")),
        other => zip_err(other),
    })?;
    let mut bytes = Vec::with_capacity(entry.size().min(64 * 1024 * 1024) as usize);
    entry.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Reads the patch record of a bundle. `Ok(None)` means: not patched by Ghostify.
pub fn read_patch_info(spa: &Path) -> Result<Option<PatchInfo>> {
    let mut archive = open_archive(spa)?;
    match read_entry(&mut archive, PATCH_INFO_ENTRY) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(Error::NotFound(_)) => Ok(None),
        Err(err) => Err(err),
    }
}

fn validate_assets(assets: &[Asset]) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for asset in assets {
        validate_entry_name(&asset.path).map_err(|e| Error::Invalid(format!("asset: {e}")))?;
        if format!("{ASSET_PREFIX}{}", asset.path) == PATCH_INFO_ENTRY {
            return Err(Error::Invalid("asset path is reserved".into()));
        }
        if !seen.insert(asset.path.as_str()) {
            return Err(Error::Invalid(format!(
                "asset `{}` listed twice",
                asset.path
            )));
        }
    }
    Ok(())
}

/// Patches `spa` in place. The bundle must be unpatched.
pub fn apply(
    spa: &Path,
    ruleset: &RuleSet,
    assets: &[Asset],
    spotify_version: Option<&SpotifyVersion>,
) -> Result<ApplyReport> {
    ruleset.validate()?;
    validate_assets(assets)?;
    if !ruleset.applies_to(spotify_version) {
        return Err(Error::Invalid(format!(
            "rule set `{}` does not support Spotify {}",
            ruleset.version,
            spotify_version.map_or("(unknown version)", SpotifyVersion::as_str)
        )));
    }

    let original_sha256 = sha256_file(spa)?;
    let mut archive = open_archive(spa)?;
    if archive
        .file_names()
        .any(|name| name.starts_with(ASSET_PREFIX))
    {
        return Err(Error::Conflict(
            "xpui.spa already contains Ghostify files; restore the original first".into(),
        ));
    }

    // Group rules per file, keeping their order.
    let mut by_file: BTreeMap<&str, Vec<&crate::rules::Rule>> = BTreeMap::new();
    for rule in &ruleset.rules {
        by_file.entry(rule.file.as_str()).or_default().push(rule);
    }

    let mut modified: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut applied = Vec::new();
    let mut skipped = Vec::new();
    for (file, rules) in by_file {
        let bytes = match read_entry(&mut archive, file) {
            Ok(bytes) => bytes,
            Err(Error::NotFound(_)) => {
                if let Some(required) = rules.iter().find(|r| r.required) {
                    return Err(Error::Conflict(format!(
                        "rule `{}` needs `{file}`, which is not in xpui.spa",
                        required.id
                    )));
                }
                skipped.extend(rules.iter().map(|r| r.id.clone()));
                continue;
            }
            Err(err) => return Err(err),
        };
        let mut text = String::from_utf8(bytes)
            .map_err(|_| Error::Invalid(format!("`{file}` is not valid UTF-8")))?;
        for rule in rules {
            match rule.apply(&text)? {
                Some(next) => {
                    text = next;
                    applied.push(rule.id.clone());
                }
                None if rule.required => {
                    return Err(Error::Conflict(format!(
                        "rule `{}` did not match in `{file}`; this Spotify version is not supported by the rule set",
                        rule.id
                    )));
                }
                None => skipped.push(rule.id.clone()),
            }
        }
        modified.insert(file.to_owned(), text.into_bytes());
    }

    let info = PatchInfo {
        ruleset_version: ruleset.version.clone(),
        ruleset_sequence: ruleset.sequence,
        spotify_version: spotify_version.map(|v| v.as_str().to_owned()),
        original_sha256,
        applied_rules: applied,
        applied_at: unix_now(),
    };

    let tmp = temp_sibling(spa);
    let written = write_patched(&mut archive, &tmp, &modified, assets, &info)
        .and_then(|()| verify(&tmp, &info, assets))
        .and_then(|()| std::fs::rename(&tmp, spa).map_err(Error::from));
    if let Err(err) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }

    Ok(ApplyReport {
        patched_sha256: sha256_file(spa)?,
        info,
        skipped_rules: skipped,
    })
}

fn write_patched(
    archive: &mut ZipArchive<File>,
    tmp: &Path,
    modified: &BTreeMap<String, Vec<u8>>,
    assets: &[Asset],
    info: &PatchInfo,
) -> Result<()> {
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut writer = ZipWriter::new(File::create(tmp)?);
    for index in 0..archive.len() {
        let name = archive
            .by_index_raw(index)
            .map_err(zip_err)?
            .name()
            .to_owned();
        match modified.get(&name) {
            Some(bytes) => {
                writer.start_file(name.as_str(), options).map_err(zip_err)?;
                writer.write_all(bytes)?;
            }
            None => {
                let entry = archive.by_index_raw(index).map_err(zip_err)?;
                writer.raw_copy_file(entry).map_err(zip_err)?;
            }
        }
    }
    for asset in assets {
        writer
            .start_file(format!("{ASSET_PREFIX}{}", asset.path), options)
            .map_err(zip_err)?;
        writer.write_all(&asset.bytes)?;
    }
    writer
        .start_file(PATCH_INFO_ENTRY, options)
        .map_err(zip_err)?;
    writer.write_all(&serde_json::to_vec_pretty(info)?)?;
    let file = writer.finish().map_err(zip_err)?;
    file.sync_all()?;
    Ok(())
}

/// Verifies a patched bundle: every entry decompresses with a valid CRC, the
/// patch record matches `expected`, and all `assets` are present unchanged.
pub fn verify(spa: &Path, expected: &PatchInfo, assets: &[Asset]) -> Result<()> {
    let mut archive = open_archive(spa)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(zip_err)?;
        std::io::copy(&mut entry, &mut std::io::sink())
            .map_err(|e| Error::Invalid(format!("xpui.spa is corrupted: {e}")))?;
    }

    let recorded: PatchInfo = serde_json::from_slice(&read_entry(&mut archive, PATCH_INFO_ENTRY)?)?;
    if recorded.ruleset_version != expected.ruleset_version
        || recorded.ruleset_sequence != expected.ruleset_sequence
        || recorded.original_sha256 != expected.original_sha256
        || recorded.applied_rules != expected.applied_rules
    {
        return Err(Error::Conflict(
            "patch record does not match the rule set".into(),
        ));
    }
    for asset in assets {
        let stored = read_entry(&mut archive, &format!("{ASSET_PREFIX}{}", asset.path))?;
        if stored != asset.bytes {
            return Err(Error::Conflict(format!(
                "asset `{}` differs from the expected content",
                asset.path
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod testutil {
    use crate::source::LoaderConfig;
    use std::io::{Read, Write};
    use std::path::Path;

    pub fn loader_config() -> LoaderConfig {
        LoaderConfig::new(vec![49421, 49422], "a".repeat(64), "test").unwrap()
    }

    /// Writes a zip bundle with the given text entries.
    pub fn make_spa(path: &Path, files: &[(&str, &str)]) {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, content) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }

    pub fn read_text(path: &Path, name: &str) -> String {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut text = String::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        text
    }

    pub const INDEX_HTML: &str = "<html><head><title>Spotify</title></head><body></body></html>";
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;
    use crate::rules::{builtin, Action, Rule};

    fn sample_spa(dir: &Path) -> std::path::PathBuf {
        let spa = dir.join("xpui.spa");
        make_spa(
            &spa,
            &[
                ("index.html", INDEX_HTML),
                ("xpui.js", "console.log('hello');"),
                ("css/xpui.css", "body{color:red}"),
            ],
        );
        spa
    }

    fn loader_assets() -> Vec<Asset> {
        vec![
            Asset {
                path: "loader.js".into(),
                bytes: b"/* loader */".to_vec(),
            },
            Asset {
                path: "loader.css".into(),
                bytes: b"/* css */".to_vec(),
            },
        ]
    }

    #[test]
    fn apply_injects_loader_adds_assets_and_keeps_other_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let spa = sample_spa(tmp.path());
        let original_sha = sha256_file(&spa).unwrap();
        assert_eq!(read_patch_info(&spa).unwrap(), None);

        let report = apply(&spa, &builtin(), &loader_assets(), None).unwrap();

        assert_eq!(report.info.original_sha256, original_sha);
        assert_eq!(report.info.applied_rules, ["inject-loader"]);
        assert_eq!(report.patched_sha256, sha256_file(&spa).unwrap());
        assert!(read_text(&spa, "index.html")
            .contains("<script defer src=\"ghostify/loader.js\"></script></head>"));
        assert_eq!(read_text(&spa, "xpui.js"), "console.log('hello');");
        assert_eq!(read_text(&spa, "css/xpui.css"), "body{color:red}");
        assert_eq!(read_text(&spa, "ghostify/loader.js"), "/* loader */");
        assert_eq!(read_patch_info(&spa).unwrap().unwrap(), report.info);
        verify(&spa, &report.info, &loader_assets()).unwrap();
        assert!(!temp_sibling(&spa).exists());
    }

    #[test]
    fn patching_twice_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let spa = sample_spa(tmp.path());
        apply(&spa, &builtin(), &loader_assets(), None).unwrap();
        let err = apply(&spa, &builtin(), &loader_assets(), None).unwrap_err();
        assert_eq!(err.code(), "conflict");
    }

    #[test]
    fn unmatched_required_rule_leaves_the_original_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let spa = tmp.path().join("xpui.spa");
        make_spa(&spa, &[("index.html", "<html><body>no head</body></html>")]);
        let before = sha256_file(&spa).unwrap();
        let err = apply(&spa, &builtin(), &loader_assets(), None).unwrap_err();
        assert_eq!(err.code(), "conflict");
        assert_eq!(sha256_file(&spa).unwrap(), before);
        assert!(!temp_sibling(&spa).exists());
    }

    #[test]
    fn missing_file_fails_only_for_required_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let spa = sample_spa(tmp.path());
        let mut set = builtin();
        set.rules.push(Rule {
            id: "optional-css".into(),
            file: "does-not-exist.css".into(),
            required: false,
            action: Action::ReplaceLiteral {
                find: "a".into(),
                replace: "b".into(),
            },
        });
        let report = apply(&spa, &set, &loader_assets(), None).unwrap();
        assert_eq!(report.skipped_rules, ["optional-css"]);

        let tmp2 = tempfile::tempdir().unwrap();
        let spa2 = sample_spa(tmp2.path());
        set.rules[1].required = true;
        let before = sha256_file(&spa2).unwrap();
        assert_eq!(
            apply(&spa2, &set, &[], None).unwrap_err().code(),
            "conflict"
        );
        assert_eq!(sha256_file(&spa2).unwrap(), before);
    }

    #[test]
    fn multiple_rules_on_one_file_apply_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let spa = sample_spa(tmp.path());
        let mut set = builtin();
        set.rules.push(Rule {
            id: "tweak-js".into(),
            file: "xpui.js".into(),
            required: true,
            action: Action::ReplaceLiteral {
                find: "hello".into(),
                replace: "ghost".into(),
            },
        });
        set.rules.push(Rule {
            id: "tweak-js-again".into(),
            file: "xpui.js".into(),
            required: true,
            action: Action::ReplaceRegex {
                pattern: "ghost".into(),
                replace: "$0!".into(),
                all: false,
            },
        });
        apply(&spa, &set, &[], None).unwrap();
        assert_eq!(read_text(&spa, "xpui.js"), "console.log('ghost!');");
    }

    #[test]
    fn unsupported_spotify_version_is_refused_before_any_work() {
        let tmp = tempfile::tempdir().unwrap();
        let spa = sample_spa(tmp.path());
        let mut set = builtin();
        set.spotify_versions.min = Some(SpotifyVersion::parse("2.0").unwrap());
        let old = SpotifyVersion::parse("1.2.31").unwrap();
        let err = apply(&spa, &set, &[], Some(&old)).unwrap_err();
        assert_eq!(err.code(), "invalid_input");
        assert_eq!(read_patch_info(&spa).unwrap(), None);
    }

    #[test]
    fn hostile_asset_paths_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let spa = sample_spa(tmp.path());
        for path in ["../evil.js", "/abs.js", "patched.json/../x", "a//b", ""] {
            let assets = [Asset {
                path: path.into(),
                bytes: vec![],
            }];
            assert_eq!(
                apply(&spa, &builtin(), &assets, None).unwrap_err().code(),
                "invalid_input",
                "{path}"
            );
        }
        let reserved = [Asset {
            path: "patched.json".into(),
            bytes: vec![],
        }];
        assert!(apply(&spa, &builtin(), &reserved, None).is_err());
        let duplicate = [
            Asset {
                path: "a.js".into(),
                bytes: vec![],
            },
            Asset {
                path: "a.js".into(),
                bytes: vec![],
            },
        ];
        assert!(apply(&spa, &builtin(), &duplicate, None).is_err());
        assert_eq!(read_patch_info(&spa).unwrap(), None);
    }

    #[test]
    fn verify_detects_changed_assets_and_corruption() {
        let tmp = tempfile::tempdir().unwrap();
        let spa = sample_spa(tmp.path());
        let report = apply(&spa, &builtin(), &loader_assets(), None).unwrap();

        let mut changed = loader_assets();
        changed[0].bytes = b"/* other loader */".to_vec();
        assert_eq!(
            verify(&spa, &report.info, &changed).unwrap_err().code(),
            "conflict"
        );

        let mut wrong = report.info.clone();
        wrong.ruleset_sequence += 1;
        assert!(verify(&spa, &wrong, &loader_assets()).is_err());

        let mut bytes = std::fs::read(&spa).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xFF;
        std::fs::write(&spa, bytes).unwrap();
        assert!(verify(&spa, &report.info, &loader_assets()).is_err());
    }

    #[test]
    fn garbage_is_not_mistaken_for_a_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let spa = tmp.path().join("xpui.spa");
        std::fs::write(&spa, b"this is not a zip file").unwrap();
        assert_eq!(read_patch_info(&spa).unwrap_err().code(), "invalid_input");
        assert!(apply(&spa, &builtin(), &[], None).is_err());
        assert_eq!(std::fs::read(&spa).unwrap(), b"this is not a zip file");
    }

    #[test]
    fn binary_entries_are_copied_without_change() {
        let tmp = tempfile::tempdir().unwrap();
        let spa = tmp.path().join("xpui.spa");
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let blob: Vec<u8> = (0..=255u8).cycle().take(100_000).collect();
        {
            let mut writer = zip::ZipWriter::new(std::fs::File::create(&spa).unwrap());
            writer.start_file("index.html", options).unwrap();
            writer.write_all(INDEX_HTML.as_bytes()).unwrap();
            writer.start_file("fonts/blob.bin", options).unwrap();
            writer.write_all(&blob).unwrap();
            writer.finish().unwrap();
        }
        apply(&spa, &builtin(), &[], None).unwrap();
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&spa).unwrap()).unwrap();
        let mut stored = Vec::new();
        archive
            .by_name("fonts/blob.bin")
            .unwrap()
            .read_to_end(&mut stored)
            .unwrap();
        assert_eq!(stored, blob);
    }
}
