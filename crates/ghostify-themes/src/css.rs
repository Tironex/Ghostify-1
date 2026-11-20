//! Turns a theme's stylesheet into one self-contained, safe stylesheet.
//!
//! * relative `@import` and `url()` references are inlined (data URIs),
//! * remote references are removed, because a theme must not phone home,
//! * constructs that execute script in old engines are refused.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ghostify_core::{Error, Result};
use regex::{Captures, Regex};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const MAX_CSS_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_COMPOSED_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ASSET_BYTES: u64 = 1024 * 1024;
const MAX_IMPORT_DEPTH: usize = 3;

/// Result of processing a stylesheet.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Processed {
    pub css: String,
    /// Human readable notes about removed or unresolved references.
    pub warnings: Vec<String>,
}

fn import_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)@import\s+(?:url\(\s*)?(?:"([^"]*)"|'([^']*)'|([^\s;)"']+))\s*\)?[^;]*;"#)
            .expect("valid regex")
    })
}

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)url\(\s*(?:"([^"]*)"|'([^']*)'|([^)"'\s]*))\s*\)"#).expect("valid regex")
    })
}

fn forbidden_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)expression\s*\(|-moz-binding|behavior\s*:|javascript\s*:|vbscript\s*:")
            .expect("valid regex")
    })
}

fn capture(c: &Captures<'_>) -> String {
    (1..=3)
        .find_map(|i| c.get(i))
        .map_or_else(String::new, |m| m.as_str().trim().to_owned())
}

fn is_remote(target: &str) -> bool {
    let lower = target.to_ascii_lowercase();
    lower.starts_with("http:")
        || lower.starts_with("https:")
        || lower.starts_with("//")
        || lower.starts_with("ftp:")
        || lower.starts_with("file:")
}

fn mime_for(path: &Path) -> Option<&'static str> {
    Some(
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "ttf" => "font/ttf",
            "otf" => "font/otf",
            _ => return None,
        },
    )
}

/// Resolves `target` below `root`, starting in `base`. Returns `None` if it escapes `root`.
fn resolve(root: &Path, base: &Path, target: &str) -> Option<PathBuf> {
    let target = target.split(['?', '#']).next().unwrap_or("");
    if target.is_empty() || target.starts_with('/') || target.contains('\\') || target.contains(':')
    {
        return None;
    }
    let joined = base.join(target).canonicalize().ok()?;
    let root = root.canonicalize().ok()?;
    joined.starts_with(&root).then_some(joined)
}

/// Processes the stylesheet at `entry` (inside `root`).
pub fn process_file(root: &Path, entry: &Path) -> Result<Processed> {
    let mut out = Processed::default();
    let mut budget = MAX_COMPOSED_BYTES;
    let css = read_css(root, entry)?;
    out.css = process(
        &css,
        root,
        entry.parent().unwrap_or(root),
        0,
        &mut out.warnings,
        &mut budget,
    )?;
    Ok(out)
}

fn read_css(root: &Path, path: &Path) -> Result<String> {
    let resolved = path.canonicalize()?;
    if !resolved.starts_with(root.canonicalize()?) {
        return Err(Error::Invalid(format!(
            "`{}` is outside of the theme folder",
            path.display()
        )));
    }
    let meta = std::fs::metadata(&resolved)?;
    if !meta.is_file() || meta.len() > MAX_CSS_BYTES as u64 {
        return Err(Error::Invalid(format!(
            "`{}` is not a stylesheet of at most {MAX_CSS_BYTES} bytes",
            path.display()
        )));
    }
    String::from_utf8(std::fs::read(resolved)?)
        .map_err(|_| Error::Invalid(format!("`{}` is not UTF-8", path.display())))
}

fn process(
    css: &str,
    root: &Path,
    base: &Path,
    depth: usize,
    warnings: &mut Vec<String>,
    budget: &mut usize,
) -> Result<String> {
    if forbidden_re().is_match(css) {
        return Err(Error::Invalid(
            "the stylesheet contains a construct that can run script".into(),
        ));
    }
    // 1. @import
    let mut failure = None;
    let imported = import_re().replace_all(css, |c: &Captures<'_>| {
        let target = capture(c);
        if is_remote(&target) {
            warnings.push(format!("removed remote @import `{target}`"));
            return String::new();
        }
        if depth >= MAX_IMPORT_DEPTH {
            warnings.push(format!("@import `{target}` nested too deeply, removed"));
            return String::new();
        }
        let Some(path) = resolve(root, base, &target) else {
            warnings.push(format!(
                "@import `{target}` not found inside the theme, removed"
            ));
            return String::new();
        };
        match read_css(root, &path).and_then(|text| {
            process(
                &text,
                root,
                path.parent().unwrap_or(root),
                depth + 1,
                warnings,
                budget,
            )
        }) {
            Ok(text) => text,
            Err(err) => {
                failure.get_or_insert(err);
                String::new()
            }
        }
    });
    if let Some(err) = failure {
        return Err(err);
    }

    // 2. url()
    let mut failure = None;
    let result = url_re().replace_all(&imported, |c: &Captures<'_>| {
        let target = capture(c);
        let lower = target.to_ascii_lowercase();
        if target.is_empty() || lower.starts_with("data:") || target.starts_with('#') {
            return c[0].to_owned();
        }
        if is_remote(&target) {
            warnings.push(format!("removed remote url `{target}`"));
            return "url(\"\")".to_owned();
        }
        let inline = (|| {
            let path = resolve(root, base, &target)?;
            let mime = mime_for(&path)?;
            let meta = std::fs::metadata(&path).ok()?;
            if !meta.is_file() || meta.len() > MAX_ASSET_BYTES {
                return None;
            }
            let bytes = std::fs::read(&path).ok()?;
            Some(format!(
                "url(\"data:{mime};base64,{}\")",
                STANDARD.encode(bytes)
            ))
        })();
        match inline {
            Some(url) => {
                if url.len() > *budget {
                    failure.get_or_insert(Error::Invalid(
                        "the theme's inlined assets are too large".into(),
                    ));
                    return String::new();
                }
                *budget -= url.len();
                url
            }
            None => {
                warnings.push(format!(
                    "url `{target}` is not a small image or font inside the theme, removed"
                ));
                "url(\"\")".to_owned()
            }
        }
    });
    if let Some(err) = failure {
        return Err(err);
    }
    let result = result.into_owned();
    if result.len() > *budget {
        return Err(Error::Invalid(
            "the composed stylesheet is too large".into(),
        ));
    }
    *budget -= result.len().min(*budget);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        dir
    }

    fn run(dir: &tempfile::TempDir) -> Result<Processed> {
        process_file(dir.path(), &dir.path().join("user.css"))
    }

    #[test]
    fn plain_css_is_unchanged() {
        let dir = theme(&[(
            "user.css",
            b"body { color: red; background: url(data:image/png;base64,AAAA) }",
        )]);
        let out = run(&dir).unwrap();
        assert_eq!(
            out.css,
            "body { color: red; background: url(data:image/png;base64,AAAA) }"
        );
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn relative_assets_are_inlined() {
        let dir = theme(&[
            (
                "user.css",
                b"a{background:url('img/bg.png')} @font-face{src:url(f.woff2)}",
            ),
            ("img/bg.png", b"PNG"),
            ("f.woff2", b"FONT"),
        ]);
        let out = run(&dir).unwrap();
        assert!(
            out.css.contains("url(\"data:image/png;base64,UE5H\")"),
            "{}",
            out.css
        );
        assert!(out.css.contains("data:font/woff2;base64,"));
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn remote_references_are_removed_with_a_warning() {
        let dir = theme(&[(
            "user.css",
            b"@import url('https://fonts.googleapis.com/css?family=X');\n@import \"//cdn.example/x.css\";\na{background:url(http://evil.example/p.png);b:url( \"HTTPS://x\" )}",
        )]);
        let out = run(&dir).unwrap();
        assert!(
            !out.css.contains("evil")
                && !out.css.contains("googleapis")
                && !out.css.contains("cdn.example"),
            "{}",
            out.css
        );
        assert_eq!(out.warnings.len(), 4);
        assert!(out.css.contains("url(\"\")"));
    }

    #[test]
    fn imports_are_inlined_recursively_with_their_own_base() {
        let dir = theme(&[
            ("user.css", b"@import 'parts/a.css';\nbody{x:1}"),
            (
                "parts/a.css",
                b"@import \"b.css\";\n.a{background:url(i.png)}",
            ),
            ("parts/b.css", b".b{y:2}"),
            ("parts/i.png", b"I"),
        ]);
        let out = run(&dir).unwrap();
        assert!(
            out.css.contains(".b{y:2}")
                && out
                    .css
                    .contains(".a{background:url(\"data:image/png;base64,SQ==\")}")
                && out.css.contains("body{x:1}")
        );
        assert!(!out.css.contains("@import"));
    }

    #[test]
    fn import_cycles_stop_at_the_depth_limit() {
        let dir = theme(&[
            ("user.css", b"@import 'a.css';x{}"),
            ("a.css", b"@import 'user.css';a{}"),
        ]);
        let out = run(&dir).unwrap();
        assert!(out.warnings.iter().any(|w| w.contains("too deeply")));
    }

    #[test]
    fn path_traversal_and_odd_files_are_refused() {
        let dir = theme(&[("user.css", b"a{b:url(../secret.png);c:url(/etc/passwd);d:url(script.js);e:url(C:\\x.png)}\n@import '../outside.css';"), ("script.js", b"x")]);
        std::fs::write(dir.path().parent().unwrap().join("secret.png"), b"s").ok();
        let out = run(&dir).unwrap();
        assert_eq!(out.css.matches("url(\"\")").count(), 4, "{}", out.css);
        assert_eq!(out.warnings.len(), 5);
    }

    #[test]
    fn oversized_assets_are_removed() {
        let big = vec![0u8; MAX_ASSET_BYTES as usize + 1];
        let dir = theme(&[
            ("user.css", b"a{background:url(big.png)}"),
            ("big.png", &big),
        ]);
        let out = run(&dir).unwrap();
        assert!(out.css.contains("url(\"\")") && out.warnings.len() == 1);
    }

    #[test]
    fn script_constructs_are_rejected() {
        for bad in [
            "a{width:expression(alert(1))}",
            "a{-moz-binding:url(x)}",
            "a{behavior:url(x.htc)}",
            "a{background:url(javascript:alert(1))}",
            "a{b:EXPRESSION (1)}",
        ] {
            let dir = theme(&[("user.css", bad.as_bytes())]);
            assert_eq!(run(&dir).unwrap_err().code(), "invalid_input", "{bad}");
        }
    }

    #[test]
    fn size_limits_apply() {
        let dir = theme(&[("user.css", &vec![b'a'; MAX_CSS_BYTES + 1])]);
        assert!(run(&dir).is_err());
        let missing = theme(&[]);
        assert!(run(&missing).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape_the_theme() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("x.png"), b"x").unwrap();
        let dir = theme(&[("user.css", b"a{background:url(link/x.png)}")]);
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        let out = run(&dir).unwrap();
        assert!(out.css.contains("url(\"\")") && out.warnings.len() == 1);
    }
}
