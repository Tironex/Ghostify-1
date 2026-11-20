//! Finding Spotify Desktop installations and reading their versions.
//!
//! All path logic works on an explicit [`Env`], so every platform layout can
//! be tested on any host.

use crate::version::SpotifyVersion;
use ghostify_db::repo::installs::InstallKind;
use std::path::{Path, PathBuf};

/// Name of the UI bundle inside Spotify's `Apps` directory.
pub const SPA_FILE: &str = "xpui.spa";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
}

impl Platform {
    pub fn current() -> Platform {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        }
    }
}

/// The directories detection looks into.
#[derive(Debug, Clone)]
pub struct Env {
    /// Root of the file system for absolute system paths (`/` on a real system).
    pub root: PathBuf,
    pub home: PathBuf,
    /// `%APPDATA%` on Windows.
    pub appdata: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
}

impl Env {
    /// Environment of the running user.
    pub fn current() -> Env {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let from_var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Env {
            root: PathBuf::from("/"),
            appdata: from_var("APPDATA"),
            xdg_config_home: from_var("XDG_CONFIG_HOME"),
            xdg_data_home: from_var("XDG_DATA_HOME"),
            home,
        }
    }

    /// Environment below an arbitrary directory, used by tests.
    pub fn rooted(root: impl Into<PathBuf>) -> Env {
        let root = root.into();
        Env {
            home: root.join("home/user"),
            appdata: Some(root.join("AppData/Roaming")),
            xdg_config_home: None,
            xdg_data_home: None,
            root,
        }
    }

    fn config_home(&self) -> PathBuf {
        self.xdg_config_home
            .clone()
            .unwrap_or_else(|| self.home.join(".config"))
    }

    fn data_home(&self) -> PathBuf {
        self.xdg_data_home
            .clone()
            .unwrap_or_else(|| self.home.join(".local/share"))
    }
}

/// A place where Spotify might be installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: InstallKind,
    pub apps_dir: PathBuf,
    pub prefs_file: Option<PathBuf>,
    pub info_plist: Option<PathBuf>,
}

/// A Spotify installation that exists on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedInstall {
    pub kind: InstallKind,
    /// Directory that contains `xpui.spa`.
    pub apps_dir: PathBuf,
    pub spa_path: PathBuf,
    pub prefs_file: Option<PathBuf>,
    pub version: Option<SpotifyVersion>,
    /// Ghostify can replace `xpui.spa`. Read-only installs (Snap, system
    /// Flatpak) cannot be patched.
    pub writable: bool,
}

/// All locations worth checking on `platform`, most specific first.
pub fn candidates(platform: Platform, env: &Env) -> Vec<Candidate> {
    match platform {
        Platform::Windows => env
            .appdata
            .iter()
            .map(|appdata| Candidate {
                kind: InstallKind::WindowsDesktop,
                apps_dir: appdata.join("Spotify").join("Apps"),
                prefs_file: Some(appdata.join("Spotify").join("prefs")),
                info_plist: None,
            })
            .collect(),
        Platform::MacOs => {
            let prefs = env.home.join("Library/Application Support/Spotify/prefs");
            [env.root.join("Applications"), env.home.join("Applications")]
                .into_iter()
                .map(|applications| {
                    let app = applications.join("Spotify.app").join("Contents");
                    Candidate {
                        kind: InstallKind::Macos,
                        apps_dir: app.join("Resources").join("Apps"),
                        prefs_file: Some(prefs.clone()),
                        info_plist: Some(app.join("Info.plist")),
                    }
                })
                .collect()
        }
        Platform::Linux => {
            let native_prefs = env.config_home().join("spotify").join("prefs");
            let mut found: Vec<Candidate> = [
                "opt/spotify/Apps",
                "usr/share/spotify/Apps",
                "usr/lib/spotify/Apps",
                "usr/lib/spotify-client/Apps",
            ]
            .iter()
            .map(|relative| Candidate {
                kind: InstallKind::LinuxNative,
                apps_dir: env.root.join(relative),
                prefs_file: Some(native_prefs.clone()),
                info_plist: None,
            })
            .collect();

            let flatpak_apps =
                "app/com.spotify.Client/current/active/files/extra/share/spotify/Apps";
            let flatpak_prefs = env
                .home
                .join(".var/app/com.spotify.Client/config/spotify/prefs");
            for base in [
                env.data_home().join("flatpak"),
                env.root.join("var/lib/flatpak"),
            ] {
                found.push(Candidate {
                    kind: InstallKind::LinuxFlatpak,
                    apps_dir: base.join(flatpak_apps),
                    prefs_file: Some(flatpak_prefs.clone()),
                    info_plist: None,
                });
            }

            found.push(Candidate {
                kind: InstallKind::LinuxSnap,
                apps_dir: env.root.join("snap/spotify/current/usr/share/spotify/Apps"),
                prefs_file: Some(env.home.join("snap/spotify/current/.config/spotify/prefs")),
                info_plist: None,
            });
            found
        }
    }
}

/// Detects installations of the running system.
pub fn detect_current() -> Vec<DetectedInstall> {
    detect(Platform::current(), &Env::current())
}

/// Detects installations on `platform`. Several candidates may point at the
/// same directory through symlinks; each real directory is reported once.
pub fn detect(platform: Platform, env: &Env) -> Vec<DetectedInstall> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut installs = Vec::new();
    for candidate in candidates(platform, env) {
        let Some(install) = probe(&candidate) else {
            continue;
        };
        let real =
            std::fs::canonicalize(&install.apps_dir).unwrap_or_else(|_| install.apps_dir.clone());
        if seen.contains(&real) {
            continue;
        }
        seen.push(real);
        installs.push(install);
    }
    installs
}

/// Inspects a user supplied `Apps` directory.
pub fn probe_custom(apps_dir: &Path, prefs_file: Option<&Path>) -> Option<DetectedInstall> {
    probe(&Candidate {
        kind: InstallKind::Custom,
        apps_dir: apps_dir.to_path_buf(),
        prefs_file: prefs_file.map(Path::to_path_buf),
        info_plist: None,
    })
}

/// Returns the installation if `candidate` really contains `xpui.spa`.
pub fn probe(candidate: &Candidate) -> Option<DetectedInstall> {
    let spa_path = candidate.apps_dir.join(SPA_FILE);
    if !spa_path.is_file() {
        return None;
    }
    let writable = is_writable(&candidate.apps_dir, &spa_path);
    let version = read_version(candidate);
    tracing::debug!(
        kind = candidate.kind.as_str(),
        dir = %candidate.apps_dir.display(),
        version = version.as_ref().map(SpotifyVersion::as_str),
        writable,
        "found Spotify installation"
    );
    Some(DetectedInstall {
        kind: candidate.kind,
        apps_dir: candidate.apps_dir.clone(),
        spa_path,
        prefs_file: candidate.prefs_file.clone(),
        version,
        writable,
    })
}

/// Checks by actually creating a file, because permission bits alone miss
/// read-only mounts and ACLs.
fn is_writable(dir: &Path, spa: &Path) -> bool {
    if std::fs::metadata(spa)
        .map(|m| m.permissions().readonly())
        .unwrap_or(true)
    {
        return false;
    }
    let probe = dir.join(format!(".ghostify-write-test-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn read_version(candidate: &Candidate) -> Option<SpotifyVersion> {
    if let Some(plist) = &candidate.info_plist {
        if let Some(version) = std::fs::read_to_string(plist)
            .ok()
            .and_then(|text| parse_plist_version(&text))
        {
            return Some(version);
        }
    }
    candidate
        .prefs_file
        .as_ref()
        .and_then(|prefs| std::fs::read_to_string(prefs).ok())
        .and_then(|text| parse_prefs_version(&text))
}

/// Reads `app.last-launched-version="1.2.31.1205.g4d59ad7c"` from Spotify's `prefs` file.
pub fn parse_prefs_version(text: &str) -> Option<SpotifyVersion> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        if key.trim() != "app.last-launched-version" {
            return None;
        }
        SpotifyVersion::parse(value.trim().trim_matches('"')).ok()
    })
}

/// Reads `CFBundleShortVersionString` from a macOS `Info.plist` in XML form.
pub fn parse_plist_version(text: &str) -> Option<SpotifyVersion> {
    let after_key = text.split_once("<key>CFBundleShortVersionString</key>")?.1;
    let value = after_key
        .split_once("<string>")?
        .1
        .split_once("</string>")?
        .0;
    SpotifyVersion::parse(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn prefs_version_is_extracted() {
        let prefs = "app.autostart-configured=true\napp.last-launched-version=\"1.2.31.1205.g4d59ad7c\"\nui.hide=false\n";
        assert_eq!(
            parse_prefs_version(prefs).unwrap().as_str(),
            "1.2.31.1205.g4d59ad7c"
        );
        assert_eq!(parse_prefs_version("app.other=1\n"), None);
        assert_eq!(
            parse_prefs_version("app.last-launched-version=\"broken\""),
            None
        );
    }

    #[test]
    fn plist_version_is_extracted() {
        let plist = "<dict><key>CFBundleName</key><string>Spotify</string>\n<key>CFBundleShortVersionString</key>\n<string>1.2.40.599</string></dict>";
        assert_eq!(parse_plist_version(plist).unwrap().as_str(), "1.2.40.599");
        assert_eq!(parse_plist_version("<dict></dict>"), None);
    }

    #[test]
    fn detects_windows_installation() {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env::rooted(tmp.path());
        let appdata = env.appdata.clone().unwrap();
        write(&appdata.join("Spotify/Apps/xpui.spa"), "zip");
        write(
            &appdata.join("Spotify/prefs"),
            "app.last-launched-version=\"1.2.50.335.gabcdef\"\n",
        );
        let found = detect(Platform::Windows, &env);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, InstallKind::WindowsDesktop);
        assert_eq!(found[0].version.as_ref().unwrap().parts(), [1, 2, 50, 335]);
        assert!(found[0].writable);
        assert_eq!(found[0].spa_path, appdata.join("Spotify/Apps/xpui.spa"));
    }

    #[test]
    fn detects_macos_installation_with_plist_version() {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env::rooted(tmp.path());
        let contents = tmp.path().join("Applications/Spotify.app/Contents");
        write(&contents.join("Resources/Apps/xpui.spa"), "zip");
        write(
            &contents.join("Info.plist"),
            "<key>CFBundleShortVersionString</key><string>1.2.41.434</string>",
        );
        let found = detect(Platform::MacOs, &env);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, InstallKind::Macos);
        assert_eq!(found[0].version.as_ref().unwrap().as_str(), "1.2.41.434");
    }

    #[test]
    fn detects_linux_native_and_flatpak_separately() {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env::rooted(tmp.path());
        write(&tmp.path().join("opt/spotify/Apps/xpui.spa"), "zip");
        write(
            &env.home.join(".config/spotify/prefs"),
            "app.last-launched-version=\"1.2.31.1205.g4d59ad7c\"\n",
        );
        write(
            &env.home.join(
                ".local/share/flatpak/app/com.spotify.Client/current/active/files/extra/share/spotify/Apps/xpui.spa",
            ),
            "zip",
        );
        let found = detect(Platform::Linux, &env);
        let kinds: Vec<_> = found.iter().map(|i| i.kind).collect();
        assert_eq!(kinds, [InstallKind::LinuxNative, InstallKind::LinuxFlatpak]);
        assert!(found[0].version.is_some());
        assert!(found[1].version.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directories_are_reported_once() {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env::rooted(tmp.path());
        write(&tmp.path().join("opt/spotify/Apps/xpui.spa"), "zip");
        std::fs::create_dir_all(tmp.path().join("usr/share")).unwrap();
        std::os::unix::fs::symlink(
            tmp.path().join("opt/spotify"),
            tmp.path().join("usr/share/spotify"),
        )
        .unwrap();
        assert_eq!(detect(Platform::Linux, &env).len(), 1);
    }

    #[test]
    fn missing_bundle_means_not_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env::rooted(tmp.path());
        std::fs::create_dir_all(tmp.path().join("opt/spotify/Apps")).unwrap();
        assert!(detect(Platform::Linux, &env).is_empty());
        assert!(detect(Platform::Windows, &env).is_empty());
    }

    #[test]
    fn custom_directory_can_be_probed() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("Apps/xpui.spa"), "zip");
        let found = probe_custom(&tmp.path().join("Apps"), None).unwrap();
        assert_eq!(found.kind, InstallKind::Custom);
        assert!(found.version.is_none());
        assert!(probe_custom(&tmp.path().join("nothing"), None).is_none());
    }

    #[test]
    fn write_probe_leaves_no_files_behind() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("Apps/xpui.spa"), "zip");
        probe_custom(&tmp.path().join("Apps"), None).unwrap();
        let names: Vec<_> = std::fs::read_dir(tmp.path().join("Apps"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["xpui.spa"]);
    }
}
