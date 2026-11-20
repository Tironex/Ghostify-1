//! Reading themes from folders and zip archives, and writing them back out.

use crate::manifest::{validate_id, ThemeManifest, MANIFEST_FILE, MAX_MANIFEST_BYTES};
use ghostify_core::{Error, Result};
use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

pub const MAX_FILES: usize = 500;
pub const MAX_TOTAL_BYTES: u64 = 20 * 1024 * 1024;
const MAX_DEPTH: usize = 6;

/// Extensions that never belong in a theme. Spicetify themes ship `theme.js`;
/// Ghostify themes are style only, scripts come as plugins with permissions.
const SKIPPED_EXTENSIONS: &[&str] = &[
    "js", "mjs", "cjs", "html", "htm", "exe", "dll", "so", "dylib", "sh", "bat", "cmd", "ps1",
];

fn skipped(name: &str) -> bool {
    name.starts_with('.')
        || Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| SKIPPED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn safe_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['\\', ':', '\0'])
        && !name.chars().any(char::is_control)
}

/// Copies a theme folder into `dst`. Returns notes about skipped files.
pub fn copy_dir(src: &Path, dst: &Path) -> Result<Vec<String>> {
    let mut state = Limits::default();
    let mut notes = Vec::new();
    copy_level(src, dst, 0, &mut state, &mut notes)?;
    Ok(notes)
}

#[derive(Default)]
struct Limits {
    files: usize,
    bytes: u64,
}

impl Limits {
    fn add(&mut self, size: u64) -> Result<()> {
        self.files += 1;
        self.bytes += size;
        if self.files > MAX_FILES || self.bytes > MAX_TOTAL_BYTES {
            return Err(Error::Invalid(format!(
                "a theme may have at most {MAX_FILES} files and {} MiB",
                MAX_TOTAL_BYTES / 1024 / 1024
            )));
        }
        Ok(())
    }
}

fn copy_level(
    src: &Path,
    dst: &Path,
    depth: usize,
    limits: &mut Limits,
    notes: &mut Vec<String>,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Invalid("theme folders are nested too deeply".into()));
    }
    fs::create_dir_all(dst)?;
    let mut entries: Vec<_> = fs::read_dir(src)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| Error::Invalid("theme file names must be UTF-8".into()))?;
        let kind = entry.file_type()?;
        if kind.is_symlink() || !safe_component(&name) {
            notes.push(format!("skipped `{name}` (link or unsafe name)"));
            continue;
        }
        if skipped(&name) {
            notes.push(format!("skipped `{name}`"));
            continue;
        }
        if kind.is_dir() {
            copy_level(&entry.path(), &dst.join(&name), depth + 1, limits, notes)?;
        } else if kind.is_file() {
            limits.add(entry.metadata()?.len())?;
            fs::copy(entry.path(), dst.join(&name))?;
        }
    }
    Ok(())
}

/// Extracts a zip into `dst`. A single wrapping folder is removed.
pub fn extract_zip(zip_path: &Path, dst: &Path) -> Result<Vec<String>> {
    let mut archive = zip::ZipArchive::new(fs::File::open(zip_path)?)
        .map_err(|e| Error::Invalid(format!("not a valid zip archive: {e}")))?;
    if archive.len() > MAX_FILES * 2 {
        return Err(Error::Invalid("the archive has too many entries".into()));
    }
    let mut names = Vec::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| Error::Invalid(format!("damaged archive: {e}")))?;
        names.push(entry.name().to_owned());
    }
    let strip = common_folder(&names);
    let mut limits = Limits::default();
    let mut notes = Vec::new();
    fs::create_dir_all(dst)?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| Error::Invalid(format!("damaged archive: {e}")))?;
        let raw = entry.name().to_owned();
        if entry.is_dir() || entry.is_symlink() {
            continue;
        }
        let relative = match &strip {
            Some(prefix) => raw.strip_prefix(prefix.as_str()).unwrap_or(&raw),
            None => &raw,
        };
        let parts: Vec<&str> = relative.split('/').collect();
        if relative.starts_with('/')
            || parts.len() > MAX_DEPTH + 1
            || !parts.iter().all(|p| safe_component(p))
        {
            return Err(Error::Invalid(format!("unsafe path in archive: `{raw}`")));
        }
        if parts.iter().any(|p| p.starts_with('.')) || parts.last().is_some_and(|p| skipped(p)) {
            notes.push(format!("skipped `{relative}`"));
            continue;
        }
        limits.add(entry.size())?;
        let target = dst.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        // Do not trust the declared size: cap what is really read.
        let mut limited = entry.by_ref().take(MAX_TOTAL_BYTES + 1);
        let mut out = fs::File::create(&target)?;
        let written = std::io::copy(&mut limited, &mut out)?;
        if written > MAX_TOTAL_BYTES {
            return Err(Error::Invalid(
                "an archive entry is larger than allowed".into(),
            ));
        }
        limits.bytes = limits.bytes.saturating_sub(entry.size()) + written;
        if limits.bytes > MAX_TOTAL_BYTES {
            return Err(Error::Invalid("the archive is larger than allowed".into()));
        }
    }
    Ok(notes)
}

/// `Some("folder/")` if every file lives in one top-level folder and the
/// theme's marker files are not at the root.
fn common_folder(names: &[String]) -> Option<String> {
    let files: Vec<&String> = names.iter().filter(|n| !n.ends_with('/')).collect();
    let first = files.first()?.split_once('/')?.0;
    if files
        .iter()
        .all(|n| n.split_once('/').is_some_and(|(top, _)| top == first))
    {
        Some(format!("{first}/"))
    } else {
        None
    }
}

/// Loads the manifest of an unpacked theme. Folders without `theme.json` but
/// with `user.css` are treated as Spicetify themes.
pub fn load_manifest(dir: &Path, fallback_name: &str) -> Result<ThemeManifest> {
    let manifest_path = dir.join(MANIFEST_FILE);
    if manifest_path.is_file() {
        if fs::metadata(&manifest_path)?.len() > MAX_MANIFEST_BYTES as u64 {
            return Err(Error::Invalid("theme.json is too large".into()));
        }
        return ThemeManifest::parse(&fs::read(manifest_path)?);
    }
    if !dir.join("user.css").is_file() {
        return Err(Error::Invalid(
            "the folder has neither theme.json nor user.css".into(),
        ));
    }
    let slug = slugify(fallback_name);
    let manifest = ThemeManifest {
        id: format!("local/{slug}"),
        name: display_name(fallback_name),
        version: "1.0.0".into(),
        author: String::new(),
        description: "Imported Spicetify theme".into(),
        css: "user.css".into(),
        color_ini: dir
            .join("color.ini")
            .is_file()
            .then(|| "color.ini".to_owned()),
        default_scheme: None,
        mode: Default::default(),
    };
    validate_id(&manifest.id)?;
    manifest.validate()?;
    Ok(manifest)
}

fn slugify(name: &str) -> String {
    let mut slug: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    slug = slug.trim_matches(|c| c == '-' || c == '.').to_owned();
    slug.truncate(40);
    if slug.is_empty() {
        "theme".into()
    } else {
        slug
    }
}

fn display_name(name: &str) -> String {
    let trimmed: String = name.chars().filter(|c| !c.is_control()).take(80).collect();
    if trimmed.trim().is_empty() {
        "Imported theme".into()
    } else {
        trimmed.trim().to_owned()
    }
}

/// Writes the theme folder as a zip with a fixed entry order.
pub fn export_zip(dir: &Path, out: &Path) -> Result<()> {
    let mut files = BTreeSet::new();
    collect(dir, Path::new(""), &mut files, 0)?;
    let tmp = out.with_extension("zip.part");
    {
        let mut zip = zip::ZipWriter::new(fs::File::create(&tmp)?);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for relative in &files {
            zip.start_file(relative, options)
                .map_err(|e| Error::Internal(format!("zip: {e}")))?;
            zip.write_all(&fs::read(dir.join(relative))?)?;
        }
        zip.finish()
            .map_err(|e| Error::Internal(format!("zip: {e}")))?;
    }
    fs::rename(&tmp, out)?;
    Ok(())
}

fn collect(base: &Path, relative: &Path, out: &mut BTreeSet<String>, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Invalid("theme folders are nested too deeply".into()));
    }
    for entry in fs::read_dir(base.join(relative))? {
        let entry = entry?;
        let child = relative.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(base, &child, out, depth + 1)?;
        } else if kind.is_file() {
            let name = child
                .components()
                .map(|c| c.as_os_str().to_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| Error::Invalid("theme file names must be UTF-8".into()))?
                .join("/");
            out.insert(name);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, bytes: &[u8]) {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn copy_skips_scripts_hidden_files_and_links() {
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "user.css", b"a{}");
        write(src.path(), "theme.js", b"alert(1)");
        write(src.path(), ".git/config", b"x");
        write(src.path(), "img/a.png", b"p");
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/passwd", src.path().join("link.css")).unwrap();
        let dst = tempfile::tempdir().unwrap();
        let notes = copy_dir(src.path(), &dst.path().join("out")).unwrap();
        assert!(
            dst.path().join("out/user.css").is_file() && dst.path().join("out/img/a.png").is_file()
        );
        assert!(!dst.path().join("out/theme.js").exists() && !dst.path().join("out/.git").exists());
        assert!(notes.iter().any(|n| n.contains("theme.js")));
        #[cfg(unix)]
        assert!(!dst.path().join("out/link.css").exists());
    }

    #[test]
    fn copy_enforces_file_count() {
        let src = tempfile::tempdir().unwrap();
        for i in 0..=MAX_FILES {
            write(src.path(), &format!("f{i}.css"), b"x");
        }
        let dst = tempfile::tempdir().unwrap();
        assert!(copy_dir(src.path(), &dst.path().join("o")).is_err());
    }

    #[test]
    fn zip_with_wrapping_folder_is_flattened() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("t.zip");
        make_zip(
            &zip,
            &[
                ("MyTheme/user.css", b"a{}"),
                ("MyTheme/color.ini", b"[A]\nx=111111"),
                ("MyTheme/theme.js", b"x"),
            ],
        );
        let dst = tmp.path().join("out");
        let notes = extract_zip(&zip, &dst).unwrap();
        assert!(
            dst.join("user.css").is_file()
                && dst.join("color.ini").is_file()
                && !dst.join("theme.js").exists()
        );
        assert!(notes.iter().any(|n| n.contains("theme.js")));
    }

    #[test]
    fn zip_slip_and_absolute_paths_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        for bad in [
            "../evil.css",
            "a/../../evil.css",
            "/abs.css",
            "a\\b.css",
            "C:/x.css",
        ] {
            let zip = tmp.path().join("bad.zip");
            make_zip(&zip, &[("user.css", b"a{}"), (bad, b"x")]);
            let dst = tmp.path().join("out");
            assert!(extract_zip(&zip, &dst).is_err(), "{bad}");
            assert!(!tmp.path().join("evil.css").exists());
            let _ = fs::remove_dir_all(&dst);
        }
    }

    #[test]
    fn broken_archives_are_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("x.zip");
        fs::write(&zip, b"not a zip").unwrap();
        assert_eq!(
            extract_zip(&zip, &tmp.path().join("o")).unwrap_err().code(),
            "invalid_input"
        );
        assert!(extract_zip(&tmp.path().join("missing.zip"), &tmp.path().join("o")).is_err());
    }

    #[test]
    fn spicetify_folders_get_a_generated_manifest() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "user.css", b"a{}");
        write(dir.path(), "color.ini", b"[A]\nx=111111");
        let m = load_manifest(dir.path(), "Dribbblish Dynamic!").unwrap();
        assert_eq!(m.id, "local/dribbblish-dynamic");
        assert_eq!(m.name, "Dribbblish Dynamic!");
        assert_eq!(m.color_ini.as_deref(), Some("color.ini"));
        assert!(load_manifest(tempfile::tempdir().unwrap().path(), "x").is_err());
        assert_eq!(load_manifest(dir.path(), "!!!").unwrap().id, "local/theme");
    }

    #[test]
    fn manifest_in_folder_wins() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "theme.json",
            br#"{"id":"me/x","name":"X","version":"2"}"#,
        );
        write(dir.path(), "user.css", b"a{}");
        assert_eq!(load_manifest(dir.path(), "ignored").unwrap().id, "me/x");
        write(dir.path(), "theme.json", b"{broken");
        assert!(load_manifest(dir.path(), "ignored").is_err());
    }

    #[test]
    fn export_round_trips_through_extract() {
        let src = tempfile::tempdir().unwrap();
        write(
            src.path(),
            "theme.json",
            br#"{"id":"me/x","name":"X","version":"1"}"#,
        );
        write(src.path(), "user.css", b"a{}");
        write(src.path(), "img/a.png", b"png");
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("x.zip");
        export_zip(src.path(), &zip).unwrap();
        assert!(!tmp.path().join("x.zip.part").exists());
        let out = tmp.path().join("out");
        extract_zip(&zip, &out).unwrap();
        assert_eq!(fs::read(out.join("img/a.png")).unwrap(), b"png");
        assert_eq!(load_manifest(&out, "x").unwrap().id, "me/x");
    }
}
