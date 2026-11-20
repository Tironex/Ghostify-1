//! File system watcher for Spotify's `Apps` directories.

use ghostify_core::{Error, Result};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// Watches `Apps` directories and emits one signal after a burst of changes
/// has been quiet for the debounce period. Dropping it stops everything.
///
/// The parent directory is watched too: when Spotify replaces the whole
/// `Apps` directory during an update, the watch on the old directory is lost,
/// and the parent still reports the replacement. After every burst the watches
/// are re-attached.
pub struct AppsWatcher {
    _watcher: Arc<Mutex<RecommendedWatcher>>,
}

fn attach(watcher: &mut RecommendedWatcher, dirs: &[PathBuf]) {
    for dir in dirs {
        let targets = [Some(dir.as_path()), dir.parent()];
        for target in targets.into_iter().flatten() {
            if target.is_dir() {
                if let Err(err) = watcher.watch(target, RecursiveMode::NonRecursive) {
                    tracing::debug!(path = %target.display(), %err, "cannot watch directory");
                }
            }
        }
    }
}

fn is_relevant(paths: &[PathBuf], dirs: &[PathBuf]) -> bool {
    paths
        .iter()
        .any(|path| dirs.iter().any(|dir| path.starts_with(dir as &Path)))
}

impl AppsWatcher {
    pub fn start(
        apps_dirs: Vec<PathBuf>,
        debounce: Duration,
    ) -> Result<(AppsWatcher, mpsc::Receiver<()>)> {
        let (raw_tx, raw_rx) = std::sync::mpsc::channel::<()>();
        let filter = apps_dirs.clone();
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                if let Ok(event) = result {
                    if !matches!(event.kind, EventKind::Access(_))
                        && is_relevant(&event.paths, &filter)
                    {
                        let _ = raw_tx.send(());
                    }
                }
            })
            .map_err(|e| Error::Internal(format!("cannot start file watcher: {e}")))?;
        attach(&mut watcher, &apps_dirs);

        let watcher = Arc::new(Mutex::new(watcher));
        let (tx, rx) = mpsc::channel(1);
        let weak = Arc::downgrade(&watcher);
        std::thread::Builder::new()
            .name("ghostify-apps-watcher".into())
            .spawn(move || {
                while raw_rx.recv().is_ok() {
                    // Wait until the burst of events is over.
                    while raw_rx.recv_timeout(debounce).is_ok() {}
                    let Some(watcher) = weak.upgrade() else { break };
                    if let Ok(mut guard) = watcher.lock() {
                        attach(&mut guard, &apps_dirs);
                    }
                    drop(watcher);
                    if let Err(mpsc::error::TrySendError::Closed(())) = tx.try_send(()) {
                        break;
                    }
                }
            })
            .map_err(|e| Error::Internal(format!("cannot start watcher thread: {e}")))?;

        Ok((AppsWatcher { _watcher: watcher }, rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    async fn signal(rx: &mut mpsc::Receiver<()>, wait: Duration) -> bool {
        matches!(tokio::time::timeout(wait, rx.recv()).await, Ok(Some(())))
    }

    #[tokio::test]
    async fn changes_in_the_apps_directory_produce_one_debounced_signal() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("Apps");
        std::fs::create_dir_all(&apps).unwrap();
        let (_watcher, mut rx) =
            AppsWatcher::start(vec![apps.clone()], Duration::from_millis(150)).unwrap();

        for i in 0..5 {
            std::fs::write(apps.join("xpui.spa"), format!("version {i}")).unwrap();
        }
        let started = Instant::now();
        assert!(
            signal(&mut rx, Duration::from_secs(5)).await,
            "no signal after a change"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "signal must wait for quiet"
        );
        assert!(
            !signal(&mut rx, Duration::from_millis(600)).await,
            "burst must collapse into one signal"
        );
    }

    #[tokio::test]
    async fn unrelated_changes_next_to_the_directory_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("Apps");
        std::fs::create_dir_all(&apps).unwrap();
        let (_watcher, mut rx) =
            AppsWatcher::start(vec![apps], Duration::from_millis(100)).unwrap();
        std::fs::write(tmp.path().join("unrelated.log"), "noise").unwrap();
        assert!(!signal(&mut rx, Duration::from_millis(700)).await);
    }

    #[tokio::test]
    async fn replacing_the_whole_directory_is_noticed_and_watching_continues() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("Apps");
        std::fs::create_dir_all(&apps).unwrap();
        let (_watcher, mut rx) =
            AppsWatcher::start(vec![apps.clone()], Duration::from_millis(100)).unwrap();

        std::fs::remove_dir_all(&apps).unwrap();
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(apps.join("xpui.spa"), "new").unwrap();
        assert!(
            signal(&mut rx, Duration::from_secs(5)).await,
            "directory replacement not noticed"
        );

        // Give the watcher a moment to re-attach, then change the new directory.
        tokio::time::sleep(Duration::from_millis(300)).await;
        while rx.try_recv().is_ok() {}
        std::fs::write(apps.join("xpui.spa"), "newer").unwrap();
        assert!(
            signal(&mut rx, Duration::from_secs(5)).await,
            "new directory is not watched"
        );
    }

    #[tokio::test]
    async fn dropping_the_watcher_closes_the_channel() {
        let tmp = tempfile::tempdir().unwrap();
        let (watcher, mut rx) =
            AppsWatcher::start(vec![tmp.path().to_path_buf()], Duration::from_millis(50)).unwrap();
        drop(watcher);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap(),
            None
        );
    }
}
