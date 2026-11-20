//! Reading plugins from folders and zip archives, hashing and archiving them.

use ghostify_core::{Error, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

pub const MAX_FILES: usize = 300;
pub const MAX_TOTAL_BYTES: u64 = 20 * 1024 * 1024;
const MAX_DEPTH: usize = 5;

/// File types a plugin may contain. Anything else is left out of the install.
const ALLOWED_EXTENSIONS: &[&str] = &[
    "js", "mjs", "css", "json", "png", "jpg", "jpeg", "gif", "webp", "svg", "woff", "woff2", "ttf",
    "otf", "txt", "md",
];

fn allowed(name: &str) -> bool {
    !name.starts_with('.')
        && Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| ALLOWED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn safe_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['\\', ':', '\0'])
        && !name.chars().any(char::is_control)
}

#[derive(Default)]
struct Limits {
    files: usize,
    bytes: u64,
}

impl Limits {
    fn add(&mut self, size: u64) -> Result<()> {
        self.files += 1;
        self.bytes = self.bytes.saturating_add(size);
        if self.files > MAX_FILES || self.bytes > MAX_TOTAL_BYTES {
            return Err(Error::Invalid(format!(
                "a plugin may have at most {MAX_FILES} files and {} MiB",
                MAX_TOTAL_BYTES / 1024 / 1024
            )));
        }
        Ok(())
    }
}

/// Copies a plugin folder; returns notes about skipped entries.
pub fn copy_dir(src: &Path, dst: &Path) -> Result<Vec<String>> {
    let mut limits = Limits::default();
    let mut notes = Vec::new();
    copy_level(src, dst, 0, &mut limits, &mut notes)?;
    Ok(notes)
}

fn copy_level(
    src: &Path,
    dst: &Path,
    depth: usize,
    limits: &mut Limits,
    notes: &mut Vec<String>,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Invalid(
            "plugin folders are nested too deeply".into(),
        ));
    }
    fs::create_dir_all(dst)?;
    let mut entries: Vec<_> = fs::read_dir(src)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| Error::Invalid("plugin file names must be UTF-8".into()))?;
        let kind = entry.file_type()?;
        if kind.is_symlink()
            || !safe_component(&name)
            || name.starts_with('.')
            || name == "node_modules"
        {
            notes.push(format!("skipped `{name}`"));
            continue;
        }
        if kind.is_dir() {
            copy_level(&entry.path(), &dst.join(&name), depth + 1, limits, notes)?;
        } else if kind.is_file() {
            if !allowed(&name) {
                notes.push(format!("skipped `{name}` (file type not allowed)"));
                continue;
            }
            limits.add(entry.metadata()?.len())?;
            fs::copy(entry.path(), dst.join(&name))?;
        }
    }
    Ok(())
}

/// Extracts a zip; a single wrapping folder is removed.
pub fn extract_zip(zip_path: &Path, dst: &Path) -> Result<Vec<String>> {
    let mut archive = zip::ZipArchive::new(fs::File::open(zip_path)?)
        .map_err(|e| Error::Invalid(format!("not a valid zip archive: {e}")))?;
    if archive.len() > MAX_FILES * 2 {
        return Err(Error::Invalid("the archive has too many entries".into()));
    }
    let mut names = Vec::new();
    for index in 0..archive.len() {
        names.push(
            archive
                .by_index(index)
                .map_err(|e| Error::Invalid(format!("damaged archive: {e}")))?
                .name()
                .to_owned(),
        );
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
        let relative = strip
            .as_deref()
            .and_then(|p| raw.strip_prefix(p))
            .unwrap_or(&raw);
        let parts: Vec<&str> = relative.split('/').collect();
        if relative.starts_with('/')
            || parts.len() > MAX_DEPTH + 1
            || !parts.iter().all(|p| safe_component(p))
        {
            return Err(Error::Invalid(format!("unsafe path in archive: `{raw}`")));
        }
        if parts.iter().any(|p| p.starts_with('.'))
            || parts.contains(&"node_modules")
            || !parts.last().is_some_and(|p| allowed(p))
        {
            notes.push(format!("skipped `{relative}`"));
            continue;
        }
        limits.add(entry.size())?;
        let target = dst.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        // The declared size is not trusted: cap what is really read.
        let mut limited = entry.by_ref().take(MAX_TOTAL_BYTES + 1);
        let written = std::io::copy(&mut limited, &mut fs::File::create(&target)?)?;
        if written > MAX_TOTAL_BYTES
            || limits
                .bytes
                .saturating_sub(entry.size())
                .saturating_add(written)
                > MAX_TOTAL_BYTES
        {
            return Err(Error::Invalid("the archive is larger than allowed".into()));
        }
    }
    Ok(notes)
}

fn common_folder(names: &[String]) -> Option<String> {
    let files: Vec<&String> = names.iter().filter(|n| !n.ends_with('/')).collect();
    let first = files.first()?.split_once('/')?.0;
    files
        .iter()
        .all(|n| n.split_once('/').is_some_and(|(top, _)| top == first))
        .then(|| format!("{first}/"))
}

/// All files of an unpacked plugin as `(relative path, content)`, sorted by path.
pub fn read_files(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut paths = BTreeSet::new();
    collect(dir, "", &mut paths, 0)?;
    let mut limits = Limits::default();
    let mut out = Vec::new();
    for relative in paths {
        let bytes = fs::read(dir.join(&relative))?;
        limits.add(bytes.len() as u64)?;
        out.push((relative, bytes));
    }
    Ok(out)
}

fn collect(base: &Path, relative: &str, out: &mut BTreeSet<String>, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH + 1 {
        return Err(Error::Invalid(
            "plugin folders are nested too deeply".into(),
        ));
    }
    for entry in fs::read_dir(base.join(relative))? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| Error::Invalid("plugin file names must be UTF-8".into()))?;
        let child = if relative.is_empty() {
            name
        } else {
            format!("{relative}/{name}")
        };
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(base, &child, out, depth + 1)?;
        } else if kind.is_file() {
            out.insert(child);
        }
    }
    Ok(())
}

/// SHA-256 over paths and contents, independent of file system order.
pub fn content_hash(files: &[(String, Vec<u8>)]) -> String {
    let mut sorted: Vec<&(String, Vec<u8>)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (path, bytes) in sorted {
        hasher.update((path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    hex(&hasher.finalize())
}

pub fn file_sha256(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut file = fs::File::open(path)?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes the plugin folder as a zip (written to a temp name, then renamed).
pub fn write_zip(dir: &Path, out: &Path) -> Result<()> {
    let files = read_files(dir)?;
    let tmp = out.with_extension("zip.part");
    {
        let mut zip = zip::ZipWriter::new(fs::File::create(&tmp)?);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (path, bytes) in &files {
            zip.start_file(path, options)
                .map_err(|e| Error::Internal(format!("zip: {e}")))?;
            zip.write_all(bytes)?;
        }
        zip.finish()
            .map_err(|e| Error::Internal(format!("zip: {e}")))?;
    }
    fs::rename(&tmp, out)?;
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
    fn copy_keeps_only_allowed_files() {
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "index.js", b"1");
        write(src.path(), "plugin.json", b"{}");
        write(src.path(), "run.exe", b"MZ");
        write(src.path(), "node_modules/x/index.js", b"1");
        write(src.path(), ".env", b"SECRET");
        write(src.path(), "img/a.png", b"p");
        let dst = tempfile::tempdir().unwrap();
        let notes = copy_dir(src.path(), &dst.path().join("o")).unwrap();
        let files: Vec<String> = read_files(&dst.path().join("o"))
            .unwrap()
            .into_iter()
            .map(|f| f.0)
            .collect();
        assert_eq!(files, ["img/a.png", "index.js", "plugin.json"]);
        assert!(
            notes.iter().any(|n| n.contains("run.exe")) && notes.iter().any(|n| n.contains(".env"))
        );
    }

    #[test]
    fn zip_extraction_is_safe_and_flattens() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("p.zip");
        make_zip(
            &zip,
            &[
                ("pkg/index.js", b"1"),
                ("pkg/plugin.json", b"{}"),
                ("pkg/setup.exe", b"x"),
            ],
        );
        let out = tmp.path().join("o");
        extract_zip(&zip, &out).unwrap();
        assert!(out.join("index.js").is_file() && !out.join("setup.exe").exists());
        for bad in ["../evil.js", "a/../../evil.js", "/abs.js", "a\\b.js"] {
            make_zip(&zip, &[("index.js", b"1"), (bad, b"x")]);
            assert!(extract_zip(&zip, &tmp.path().join("o2")).is_err(), "{bad}");
        }
        fs::write(&zip, b"junk").unwrap();
        assert!(extract_zip(&zip, &tmp.path().join("o3")).is_err());
    }

    #[test]
    fn hash_depends_on_paths_and_contents_not_order() {
        let a = vec![
            ("a.js".to_owned(), b"1".to_vec()),
            ("b.js".to_owned(), b"2".to_vec()),
        ];
        let b = vec![
            ("b.js".to_owned(), b"2".to_vec()),
            ("a.js".to_owned(), b"1".to_vec()),
        ];
        assert_eq!(content_hash(&a), content_hash(&b));
        let renamed = vec![
            ("a.js".to_owned(), b"1".to_vec()),
            ("c.js".to_owned(), b"2".to_vec()),
        ];
        let changed = vec![
            ("a.js".to_owned(), b"1".to_vec()),
            ("b.js".to_owned(), b"3".to_vec()),
        ];
        assert_ne!(content_hash(&a), content_hash(&renamed));
        assert_ne!(content_hash(&a), content_hash(&changed));
        // boundaries matter: ("ab","c") differs from ("a","bc")
        let x = vec![("ab".to_owned(), b"c".to_vec())];
        let y = vec![("a".to_owned(), b"bc".to_vec())];
        assert_ne!(content_hash(&x), content_hash(&y));
        assert_eq!(content_hash(&[]).len(), 64);
    }

    #[test]
    fn zip_round_trip_and_file_hash() {
        let src = tempfile::tempdir().unwrap();
        write(src.path(), "plugin.json", b"{}");
        write(src.path(), "lib/a.js", b"alert");
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("x.zip");
        write_zip(src.path(), &zip).unwrap();
        assert!(!tmp.path().join("x.zip.part").exists());
        let out = tmp.path().join("o");
        extract_zip(&zip, &out).unwrap();
        assert_eq!(
            content_hash(&read_files(&out).unwrap()),
            content_hash(&read_files(src.path()).unwrap())
        );
        assert_eq!(file_sha256(&zip).unwrap().len(), 64);
        assert!(file_sha256(&tmp.path().join("none")).is_err());
    }

    #[test]
    fn too_many_files_are_refused() {
        let src = tempfile::tempdir().unwrap();
        for i in 0..=MAX_FILES {
            write(src.path(), &format!("f{i}.js"), b"1");
        }
        assert!(copy_dir(src.path(), &tempfile::tempdir().unwrap().path().join("o")).is_err());
    }
}
