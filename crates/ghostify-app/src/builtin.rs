//! Themes that ship with Ghostify.
//!
//! The "Ghost" theme is installed once, on first start. It becomes the active
//! theme only if the user has none yet, and it is never reinstalled afterwards,
//! so removing it or choosing another theme is respected.

use ghostify_core::{Error, Result};
use ghostify_db::repo::themes::ThemeSource;
use ghostify_themes::ThemeStore;
use std::fs;
use std::path::Path;

pub const GHOST_ID: &str = "ghostify/ghost";

const GHOST_FILES: [(&str, &str); 3] = [
    ("theme.json", include_str!("../../../themes/ghostify-ghost/theme.json")),
    ("user.css", include_str!("../../../themes/ghostify-ghost/user.css")),
    ("color.ini", include_str!("../../../themes/ghostify-ghost/color.ini")),
];

/// Installs the bundled themes that are not known yet. Returns the ids installed.
pub fn install_builtin_themes(store: &ThemeStore, staging: &Path) -> Result<Vec<String>> {
    match store.info(GHOST_ID) {
        Ok(_) => return Ok(Vec::new()),
        Err(Error::NotFound(_)) => {}
        Err(other) => return Err(other),
    }
    let dir = staging.join("builtin-ghost");
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    fs::create_dir_all(&dir)?;
    for (name, body) in GHOST_FILES {
        fs::write(dir.join(name), body)?;
    }
    let installed = store.install_dir(&dir, ThemeSource::Local, false);
    // The staging copy is only needed for the install itself.
    let _ = fs::remove_dir_all(&dir);
    let installed = installed?;
    if store.active()?.is_none() {
        store.activate(&installed.info.theme.id, None)?;
    }
    Ok(vec![GHOST_ID.to_owned()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghostify_core::AppPaths;
    use ghostify_db::Db;

    fn store(root: &Path) -> ThemeStore {
        ThemeStore::new(Db::open_in_memory().unwrap(), &AppPaths::from_root(root)).unwrap()
    }

    #[test]
    fn installs_once_and_activates_when_nothing_is_active() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(tmp.path());
        let staging = tmp.path().join("staging");
        assert_eq!(install_builtin_themes(&store, &staging).unwrap(), vec![GHOST_ID]);
        assert_eq!(store.active().unwrap().unwrap().id, GHOST_ID);
        let composed = store.compose(GHOST_ID, None).unwrap();
        assert!(composed.css.contains("--spice-main:#050505"));
        assert!(composed.css.contains("--ghost-radius"));
        assert!(composed.warnings.is_empty(), "{:?}", composed.warnings);
        assert!(install_builtin_themes(&store, &staging).unwrap().is_empty());
    }

    #[test]
    fn a_deactivated_theme_is_not_activated_again() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(tmp.path());
        let staging = tmp.path().join("staging");
        install_builtin_themes(&store, &staging).unwrap();
        store.deactivate().unwrap();
        install_builtin_themes(&store, &staging).unwrap();
        assert!(store.active().unwrap().is_none());
    }

    #[test]
    fn existing_active_theme_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(tmp.path());
        let other = tmp.path().join("other");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("theme.json"), r#"{"id":"me/mine","name":"Mine","version":"1.0.0"}"#).unwrap();
        fs::write(other.join("user.css"), "body{color:red}").unwrap();
        store.install_dir(&other, ThemeSource::Local, false).unwrap();
        store.activate("me/mine", None).unwrap();
        install_builtin_themes(&store, &tmp.path().join("staging")).unwrap();
        assert_eq!(store.active().unwrap().unwrap().id, "me/mine");
    }
}
