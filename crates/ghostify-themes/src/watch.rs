//! Debounced watcher for one theme folder.

use ghostify_core::{Error, Result};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Emits one signal after a burst of changes below the folder has been quiet
/// for `debounce`. Dropping the value stops the watcher and its thread.
pub struct ThemeWatcher {
    _watcher: RecommendedWatcher,
}

impl ThemeWatcher {
    pub fn start(dir: &Path, debounce: Duration) -> Result<(ThemeWatcher, mpsc::Receiver<()>)> {
        let (raw_tx, raw_rx) = std_mpsc::channel::<()>();
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                if let Ok(event) = result {
                    if !matches!(event.kind, EventKind::Access(_)) {
                        let _ = raw_tx.send(());
                    }
                }
            })
            .map_err(|e| Error::Internal(format!("cannot start file watcher: {e}")))?;
        watcher
            .watch(dir, RecursiveMode::Recursive)
            .map_err(|e| Error::Internal(format!("cannot watch `{}`: {e}", dir.display())))?;

        let (tx, rx) = mpsc::channel::<()>(1);
        std::thread::Builder::new()
            .name("ghostify-theme-watch".into())
            .spawn(move || {
                // Ends when the watcher (and with it `raw_tx`) is dropped.
                while raw_rx.recv().is_ok() {
                    loop {
                        match raw_rx.recv_timeout(debounce) {
                            Ok(()) => continue,
                            Err(std_mpsc::RecvTimeoutError::Timeout) => break,
                            Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    // A full channel already holds a pending signal.
                    if let Err(mpsc::error::TrySendError::Closed(())) = tx.try_send(()) {
                        return;
                    }
                }
            })?;
        Ok((ThemeWatcher { _watcher: watcher }, rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::timeout;

    #[tokio::test]
    async fn bursts_collapse_into_one_signal() {
        let dir = tempfile::tempdir().unwrap();
        let (_watcher, mut rx) =
            ThemeWatcher::start(dir.path(), Duration::from_millis(150)).unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join("user.css"), format!("a{{x:{i}}}")).unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("signal")
            .expect("open");
        assert!(
            timeout(Duration::from_millis(500), rx.recv())
                .await
                .is_err(),
            "only one signal per burst"
        );
    }

    #[tokio::test]
    async fn nested_changes_are_seen() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("img")).unwrap();
        let (_watcher, mut rx) =
            ThemeWatcher::start(dir.path(), Duration::from_millis(50)).unwrap();
        std::fs::write(dir.path().join("img/a.png"), b"x").unwrap();
        timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("signal")
            .expect("open");
    }

    #[test]
    fn missing_folder_is_an_error() {
        assert!(
            ThemeWatcher::start(Path::new("/definitely/not/here"), Duration::from_millis(10))
                .is_err()
        );
    }
}
