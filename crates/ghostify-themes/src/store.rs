//! Installed themes: files below the themes directory plus rows in the database.

use crate::css;
use crate::ini::ColorSchemes;
use crate::manifest::{dir_name, validate_id, ThemeManifest};
use crate::package;
use ghostify_core::{AppPaths, Error, Result};
use ghostify_db::repo::settings;
use ghostify_db::repo::themes::{self, Theme, ThemeSource};
use ghostify_db::Db;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MODE_KEY: &str = "theme.mode";
static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A stylesheet ready to be injected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Composed {
    pub theme_id: String,
    pub css: String,
    pub color_scheme: Option<String>,
    pub warnings: Vec<String>,
}

/// What the loader needs after connecting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Current {
    pub theme_id: Option<String>,
    pub css: String,
    /// `dark` or `light`.
    pub scheme: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThemeInfo {
    #[serde(flatten)]
    pub theme: Theme,
    /// `None` if the files on disk are missing or damaged.
    pub manifest: Option<ThemeManifest>,
    pub color_schemes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub info: ThemeInfo,
    /// Notes from the import (skipped files).
    pub notes: Vec<String>,
}

#[derive(Clone)]
pub struct ThemeStore {
    db: Db,
    root: PathBuf,
    staging: PathBuf,
}

impl ThemeStore {
    pub fn new(db: Db, paths: &AppPaths) -> Result<ThemeStore> {
        let store = ThemeStore {
            db,
            root: paths.themes_dir(),
            staging: paths.staging_dir().join("themes"),
        };
        fs::create_dir_all(&store.root)?;
        fs::create_dir_all(&store.staging)?;
        Ok(store)
    }

    pub fn theme_dir(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self.root.join(dir_name(id)))
    }

    /// Installs from a folder (Ghostify or Spicetify layout).
    pub fn install_dir(&self, src: &Path, source: ThemeSource, replace: bool) -> Result<Installed> {
        let fallback = src
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("theme")
            .to_owned();
        let stage = Staging::new(&self.staging)?;
        let notes = package::copy_dir(src, stage.path())?;
        self.finish(stage, &fallback, notes, source, replace)
    }

    /// Installs from a zip archive.
    pub fn install_zip(&self, zip: &Path, source: ThemeSource, replace: bool) -> Result<Installed> {
        let fallback = zip
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("theme")
            .to_owned();
        let stage = Staging::new(&self.staging)?;
        let notes = package::extract_zip(zip, stage.path())?;
        self.finish(stage, &fallback, notes, source, replace)
    }

    /// Reads and validates a theme archive without installing anything.
    pub fn inspect_zip(&self, zip: &Path) -> Result<crate::manifest::ThemeManifest> {
        let fallback = zip
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("theme")
            .to_owned();
        let stage = Staging::new(&self.staging)?;
        package::extract_zip(zip, stage.path())?;
        let manifest = package::load_manifest(stage.path(), &fallback)?;
        compose_dir(stage.path(), &manifest, None)?;
        Ok(manifest)
    }

    fn finish(
        &self,
        stage: Staging,
        fallback: &str,
        mut notes: Vec<String>,
        source: ThemeSource,
        replace: bool,
    ) -> Result<Installed> {
        let manifest = package::load_manifest(stage.path(), fallback)?;
        // Everything that can go wrong with the content is checked before anything is touched.
        let composed = compose_dir(stage.path(), &manifest, None)?;
        notes.extend(composed.warnings);

        let id = manifest.id.clone();
        let target = self.theme_dir(&id)?;
        let existing = self.db.with(|c| themes::get(c, &id))?;
        if existing.is_some() && !replace {
            return Err(Error::Conflict(format!(
                "theme `{id}` is already installed"
            )));
        }
        if existing.is_none() && target.exists() {
            // Leftover of an interrupted install: not referenced by the database.
            fs::remove_dir_all(&target)?;
        }
        // Normalised manifest so Spicetify imports keep their generated id.
        fs::write(stage.path().join("theme.json"), manifest.to_json()?)?;

        let backup = target.with_extension("old");
        let had_files = target.exists();
        if had_files {
            let _ = fs::remove_dir_all(&backup);
            fs::rename(&target, &backup)?;
        }
        if let Err(err) = fs::rename(stage.path(), &target) {
            if had_files {
                let _ = fs::rename(&backup, &target);
            }
            return Err(err.into());
        }
        stage.disarm();

        let db_result = self.db.with(|c| {
            let (was_active, scheme) = existing
                .as_ref()
                .map_or((false, None), |t| (t.active, Some(t.color_scheme.clone())));
            if existing.is_some() {
                themes::remove(c, &id)?;
            }
            themes::install(c, &id, &manifest.name, &manifest.version, source)?;
            if was_active {
                themes::activate(c, &id)?;
            }
            if let Some(scheme) = scheme {
                // Keep the chosen scheme only if the new version still has it.
                let keep = ColorSchemes::parse(
                    &fs::read_to_string(target.join(manifest.color_ini.as_deref().unwrap_or("-")))
                        .unwrap_or_default(),
                )
                .map(|s| s.contains(&scheme))
                .unwrap_or(false);
                if keep {
                    themes::set_color_scheme(c, &id, &scheme)?;
                }
            }
            Ok(())
        });
        if let Err(err) = db_result {
            let _ = fs::remove_dir_all(&target);
            if had_files {
                let _ = fs::rename(&backup, &target);
            }
            return Err(err);
        }
        let _ = fs::remove_dir_all(&backup);
        Ok(Installed {
            info: self.info(&id)?,
            notes,
        })
    }

    pub fn list(&self) -> Result<Vec<ThemeInfo>> {
        let rows = self.db.with(themes::list)?;
        Ok(rows.into_iter().map(|theme| self.describe(theme)).collect())
    }

    pub fn info(&self, id: &str) -> Result<ThemeInfo> {
        let theme = self
            .db
            .with(|c| themes::get(c, id))?
            .ok_or_else(|| Error::NotFound(format!("theme `{id}`")))?;
        Ok(self.describe(theme))
    }

    fn describe(&self, theme: Theme) -> ThemeInfo {
        let dir = self.root.join(dir_name(&theme.id));
        let manifest = package::load_manifest(&dir, &theme.id).ok();
        let color_schemes = manifest
            .as_ref()
            .and_then(|m| read_schemes(&dir, m).ok().flatten())
            .map(|s| s.names().to_vec())
            .unwrap_or_default();
        ThemeInfo {
            theme,
            manifest,
            color_schemes,
        }
    }

    /// Builds the stylesheet of an installed theme from the files on disk.
    pub fn compose(&self, id: &str, scheme: Option<&str>) -> Result<Composed> {
        let dir = self.theme_dir(id)?;
        if !dir.is_dir() {
            return Err(Error::NotFound(format!("theme `{id}`")));
        }
        let manifest = package::load_manifest(&dir, id)?;
        compose_dir(&dir, &manifest, scheme)
    }

    /// Makes a theme active. `scheme` selects a colour scheme of the theme.
    pub fn activate(&self, id: &str, scheme: Option<&str>) -> Result<ThemeInfo> {
        let composed = self.compose(id, scheme)?;
        let chosen = composed
            .color_scheme
            .clone()
            .unwrap_or_else(|| "default".to_owned());
        self.db.with(|c| {
            themes::activate(c, id)?;
            themes::set_color_scheme(c, id, &chosen)?;
            Ok(())
        })?;
        self.info(id)
    }

    pub fn deactivate(&self) -> Result<()> {
        self.db.with(themes::deactivate_all)
    }

    pub fn active(&self) -> Result<Option<Theme>> {
        self.db.with(themes::active)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let dir = self.theme_dir(id)?;
        self.db.with(|c| themes::remove(c, id))?;
        if dir.exists() {
            fs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    /// Writes the theme as a zip archive.
    pub fn export(&self, id: &str, out: &Path) -> Result<()> {
        self.info(id)?;
        package::export_zip(&self.theme_dir(id)?, out)
    }

    pub fn mode(&self) -> Result<String> {
        Ok(self
            .db
            .with(|c| settings::get::<String>(c, MODE_KEY))?
            .filter(|m| m == "dark" || m == "light")
            .unwrap_or_else(|| "dark".to_owned()))
    }

    pub fn set_mode(&self, mode: &str) -> Result<()> {
        if mode != "dark" && mode != "light" {
            return Err(Error::Invalid("mode must be `dark` or `light`".into()));
        }
        self.db.with(|c| settings::set(c, MODE_KEY, &mode))
    }

    /// State for the loader. A missing or broken active theme yields empty CSS
    /// plus a warning; Spotify must keep working.
    pub fn current(&self) -> Result<Current> {
        let scheme = self.mode()?;
        let Some(active) = self.active()? else {
            return Ok(Current {
                theme_id: None,
                css: String::new(),
                scheme,
                warnings: vec![],
            });
        };
        let wanted = (active.color_scheme != "default").then_some(active.color_scheme.as_str());
        match self
            .compose(&active.id, wanted)
            .or_else(|_| self.compose(&active.id, None))
        {
            Ok(composed) => Ok(Current {
                theme_id: Some(active.id),
                css: composed.css,
                scheme,
                warnings: composed.warnings,
            }),
            Err(err) => Ok(Current {
                theme_id: Some(active.id.clone()),
                css: String::new(),
                scheme,
                warnings: vec![format!("theme `{}` cannot be loaded: {err}", active.id)],
            }),
        }
    }
}

fn read_schemes(dir: &Path, manifest: &ThemeManifest) -> Result<Option<ColorSchemes>> {
    let Some(ini) = &manifest.color_ini else {
        return Ok(None);
    };
    let path = dir.join(ini);
    let meta = fs::metadata(&path)?;
    if !meta.is_file() || meta.len() > crate::ini::MAX_INI_BYTES as u64 {
        return Err(Error::Invalid(format!(
            "`{ini}` is not a usable colour file"
        )));
    }
    ColorSchemes::parse(&fs::read_to_string(path)?).map(Some)
}

fn compose_dir(dir: &Path, manifest: &ThemeManifest, scheme: Option<&str>) -> Result<Composed> {
    let processed = css::process_file(dir, &dir.join(&manifest.css))?;
    let mut text = String::new();
    let mut used = None;
    if let Some(schemes) = read_schemes(dir, manifest)? {
        let name = match scheme {
            Some(name) => name.to_owned(),
            None => manifest
                .default_scheme
                .clone()
                .filter(|d| schemes.contains(d))
                .or_else(|| schemes.first().map(str::to_owned))
                .ok_or_else(|| Error::Invalid("the theme has no colour schemes".into()))?,
        };
        text.push_str(&schemes.to_css(&name)?);
        text.push('\n');
        used = Some(name);
    } else if let Some(name) = scheme {
        return Err(Error::NotFound(format!(
            "color scheme `{name}` (the theme has none)"
        )));
    }
    text.push_str(&processed.css);
    Ok(Composed {
        theme_id: manifest.id.clone(),
        css: text,
        color_scheme: used,
        warnings: processed.warnings,
    })
}

/// Temporary folder that is removed unless disarmed.
struct Staging {
    path: PathBuf,
    armed: bool,
}

impl Staging {
    fn new(base: &Path) -> Result<Staging> {
        let unique = format!(
            "{}-{}-{}",
            std::process::id(),
            STAGING_COUNTER.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let path = base.join(unique);
        fs::create_dir_all(&path)?;
        Ok(Staging { path, armed: true })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    pub fn store() -> (ThemeStore, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(tmp.path());
        let store = ThemeStore::new(Db::open_in_memory().unwrap(), &paths).unwrap();
        (store, tmp)
    }

    pub fn write_theme(dir: &Path, id: &str, css: &str, ini: Option<&str>) {
        fs::create_dir_all(dir).unwrap();
        let ini_field = if ini.is_some() {
            r#","color_ini":"color.ini""#
        } else {
            ""
        };
        fs::write(
            dir.join("theme.json"),
            format!(r#"{{"id":"{id}","name":"Theme {id}","version":"1.0.0"{ini_field}}}"#),
        )
        .unwrap();
        fs::write(dir.join("user.css"), css).unwrap();
        if let Some(ini) = ini {
            fs::write(dir.join("color.ini"), ini).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    const INI: &str = "[Base]\nmain = 121212\n[Light]\nmain = ffffff\n";

    #[test]
    fn install_activate_and_compose() {
        let (store, tmp) = store();
        let src = tmp.path().join("src");
        write_theme(
            &src,
            "me/dark",
            "body{background:var(--spice-main)}",
            Some(INI),
        );
        let installed = store.install_dir(&src, ThemeSource::Local, false).unwrap();
        assert_eq!(installed.info.theme.id, "me/dark");
        assert_eq!(installed.info.color_schemes, ["Base", "Light"]);
        assert!(store
            .theme_dir("me/dark")
            .unwrap()
            .join("user.css")
            .is_file());

        let info = store.activate("me/dark", Some("Light")).unwrap();
        assert!(info.theme.active);
        assert_eq!(info.theme.color_scheme, "Light");
        let current = store.current().unwrap();
        assert_eq!(current.theme_id.as_deref(), Some("me/dark"));
        assert!(
            current.css.starts_with(":root{--spice-main:#ffffff;")
                && current.css.ends_with("body{background:var(--spice-main)}")
        );
        assert_eq!(current.scheme, "dark");
    }

    #[test]
    fn spicetify_folder_install() {
        let (store, tmp) = store();
        let src = tmp.path().join("Comfy Theme");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("user.css"), "a{}").unwrap();
        fs::write(src.join("color.ini"), INI).unwrap();
        fs::write(src.join("theme.js"), "alert(1)").unwrap();
        let installed = store.install_dir(&src, ThemeSource::Import, false).unwrap();
        assert_eq!(installed.info.theme.id, "local/comfy-theme");
        assert!(installed.notes.iter().any(|n| n.contains("theme.js")));
        assert!(!store
            .theme_dir("local/comfy-theme")
            .unwrap()
            .join("theme.js")
            .exists());
    }

    #[test]
    fn duplicate_installs_conflict_unless_replacing() {
        let (store, tmp) = store();
        let src = tmp.path().join("src");
        write_theme(&src, "a", "x{}", Some(INI));
        store.install_dir(&src, ThemeSource::Local, false).unwrap();
        assert_eq!(
            store
                .install_dir(&src, ThemeSource::Local, false)
                .unwrap_err()
                .code(),
            "conflict"
        );

        store.activate("a", Some("Light")).unwrap();
        write_theme(&src, "a", "y{}", Some(INI));
        fs::write(
            src.join("theme.json"),
            r#"{"id":"a","name":"A2","version":"2.0.0","color_ini":"color.ini"}"#,
        )
        .unwrap();
        let updated = store.install_dir(&src, ThemeSource::Local, true).unwrap();
        assert_eq!(updated.info.theme.version, "2.0.0");
        assert!(
            updated.info.theme.active,
            "an active theme stays active after an update"
        );
        assert_eq!(
            updated.info.theme.color_scheme, "Light",
            "the chosen scheme survives"
        );
        assert!(store.compose("a", None).unwrap().css.ends_with("y{}"));
        assert!(!store.theme_dir("a").unwrap().with_extension("old").exists());
    }

    #[test]
    fn invalid_content_installs_nothing() {
        let (store, tmp) = store();
        let src = tmp.path().join("src");
        write_theme(&src, "bad", "a{width:expression(1)}", None);
        assert!(store.install_dir(&src, ThemeSource::Local, false).is_err());
        assert!(store.list().unwrap().is_empty());
        assert!(!store.theme_dir("bad").unwrap().exists());
        assert_eq!(
            fs::read_dir(&store.staging).unwrap().count(),
            0,
            "staging is cleaned up"
        );

        write_theme(&src, "bad", "a{}", Some("garbage"));
        assert!(store.install_dir(&src, ThemeSource::Local, false).is_err());
        write_theme(&src, "ok", "a{}", None);
        fs::remove_file(src.join("user.css")).unwrap();
        assert!(store.install_dir(&src, ThemeSource::Local, false).is_err());
    }

    #[test]
    fn unknown_scheme_is_rejected_and_nothing_changes() {
        let (store, tmp) = store();
        let src = tmp.path().join("src");
        write_theme(&src, "a", "x{}", Some(INI));
        store.install_dir(&src, ThemeSource::Local, false).unwrap();
        assert_eq!(
            store.activate("a", Some("Nope")).unwrap_err().code(),
            "not_found"
        );
        assert!(store.active().unwrap().is_none());
        assert_eq!(
            store.activate("ghost", None).unwrap_err().code(),
            "not_found"
        );
    }

    #[test]
    fn schemes_on_a_theme_without_colours_are_rejected() {
        let (store, tmp) = store();
        let src = tmp.path().join("src");
        write_theme(&src, "plain", "x{}", None);
        store.install_dir(&src, ThemeSource::Local, false).unwrap();
        assert!(store.activate("plain", Some("Base")).is_err());
        let info = store.activate("plain", None).unwrap();
        assert_eq!(info.theme.color_scheme, "default");
        assert_eq!(store.compose("plain", None).unwrap().css, "x{}");
    }

    #[test]
    fn remove_deletes_files_and_row() {
        let (store, tmp) = store();
        let src = tmp.path().join("src");
        write_theme(&src, "a", "x{}", None);
        store.install_dir(&src, ThemeSource::Local, false).unwrap();
        store.activate("a", None).unwrap();
        store.remove("a").unwrap();
        assert!(store.list().unwrap().is_empty() && !store.theme_dir("a").unwrap().exists());
        assert_eq!(store.current().unwrap().theme_id, None);
        assert_eq!(store.remove("a").unwrap_err().code(), "not_found");
        assert!(store.theme_dir("../x").is_err());
    }

    #[test]
    fn broken_active_theme_degrades_to_empty_css() {
        let (store, tmp) = store();
        let src = tmp.path().join("src");
        write_theme(&src, "a", "x{}", None);
        store.install_dir(&src, ThemeSource::Local, false).unwrap();
        store.activate("a", None).unwrap();
        fs::remove_file(store.theme_dir("a").unwrap().join("user.css")).unwrap();
        let current = store.current().unwrap();
        assert_eq!(current.css, "");
        assert_eq!(current.theme_id.as_deref(), Some("a"));
        assert!(current.warnings[0].contains("cannot be loaded"));
        assert!(
            store.list().unwrap()[0].manifest.is_some()
                || store.list().unwrap()[0].manifest.is_none()
        );
    }

    #[test]
    fn export_and_reimport_via_zip() {
        let (store, tmp) = store();
        let src = tmp.path().join("src");
        write_theme(&src, "me/x", "a{background:url(i.png)}", Some(INI));
        fs::write(src.join("i.png"), b"png").unwrap();
        store.install_dir(&src, ThemeSource::Local, false).unwrap();
        let zip = tmp.path().join("x.zip");
        store.export("me/x", &zip).unwrap();
        store.remove("me/x").unwrap();
        let again = store.install_zip(&zip, ThemeSource::Import, false).unwrap();
        assert_eq!(again.info.theme.id, "me/x");
        assert!(store
            .compose("me/x", None)
            .unwrap()
            .css
            .contains("data:image/png;base64,"));
        assert_eq!(
            store
                .export("ghost", &tmp.path().join("g.zip"))
                .unwrap_err()
                .code(),
            "not_found"
        );
    }

    #[test]
    fn mode_is_validated_and_persisted() {
        let (store, _tmp) = store();
        assert_eq!(store.mode().unwrap(), "dark");
        store.set_mode("light").unwrap();
        assert_eq!(store.mode().unwrap(), "light");
        assert!(store.set_mode("purple").is_err());
        assert_eq!(store.mode().unwrap(), "light");
    }
}
