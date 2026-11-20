//! File helpers: hashing, atomic replacement and zip error mapping.

use ghostify_core::{Error, Result};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn to_hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

pub fn sha256_bytes(data: &[u8]) -> String {
    to_hex(&Sha256::digest(data))
}

/// SHA-256 of a file, streamed so that large files never sit in memory.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(to_hex(&hasher.finalize()))
}

/// Temporary name next to `path`, on the same file system so that a rename is atomic.
pub fn temp_sibling(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".ghostify-tmp");
    path.with_file_name(name)
}

/// Copies `src` to `dest` through a temporary file that is flushed to disk
/// and then renamed over `dest`. Returns the SHA-256 of the bytes written, so
/// the caller can compare it with an expected hash. A failed copy never
/// leaves a partial `dest` behind.
pub fn copy_verified(src: &Path, dest: &Path) -> Result<String> {
    let tmp = temp_sibling(dest);
    let copied = (|| -> Result<String> {
        let mut input = File::open(src)?;
        let mut output = File::create(&tmp)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read])?;
            hasher.update(&buffer[..read]);
        }
        output.sync_all()?;
        Ok(to_hex(&hasher.finalize()))
    })();
    match copied {
        Ok(sha) => {
            if let Err(err) = std::fs::rename(&tmp, dest) {
                let _ = std::fs::remove_file(&tmp);
                return Err(err.into());
            }
            Ok(sha)
        }
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            Err(err)
        }
    }
}

pub(crate) fn zip_err(err: zip::result::ZipError) -> Error {
    match err {
        zip::result::ZipError::Io(io) => Error::Io(io),
        zip::result::ZipError::FileNotFound => Error::NotFound("entry in xpui.spa".into()),
        other => Error::Invalid(format!("invalid xpui.spa: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_match_known_vectors() {
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(sha256_file(&path).unwrap(), sha256_bytes(b"abc"));
    }

    #[test]
    fn copy_verified_replaces_destination_and_reports_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dest = tmp.path().join("dest");
        std::fs::write(&src, vec![7u8; 200_000]).unwrap();
        std::fs::write(&dest, b"old").unwrap();
        let sha = copy_verified(&src, &dest).unwrap();
        assert_eq!(sha, sha256_file(&dest).unwrap());
        assert_eq!(std::fs::read(&dest).unwrap().len(), 200_000);
        assert!(!temp_sibling(&dest).exists());
    }

    #[test]
    fn failed_copy_keeps_the_destination_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        std::fs::write(&dest, b"keep me").unwrap();
        assert!(copy_verified(&tmp.path().join("missing"), &dest).is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"keep me");
        assert!(!temp_sibling(&dest).exists());
    }
}
