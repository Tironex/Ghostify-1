//! Automatic repatch: runs a repair pass at start-up and whenever Spotify's
//! files change.

use crate::manager::PatchManager;
use crate::watch::AppsWatcher;
use ghostify_core::Result;
use std::path::PathBuf;
use std::time::Duration;
use tokio::task::JoinHandle;

/// Background service. Dropping it (or calling [`AutoRepatch::stop`]) ends it.
pub struct AutoRepatch {
    task: JoinHandle<()>,
    _watcher: AppsWatcher,
}

impl AutoRepatch {
    /// Detects installations, starts watching their `Apps` directories and
    /// repairs patched installations now and after every change.
    ///
    /// Repairs are idempotent: the changes a repair makes itself trigger one
    /// more pass, which finds everything healthy and writes nothing.
    pub async fn start(manager: PatchManager, debounce: Duration) -> Result<AutoRepatch> {
        let installs = manager.detect_and_sync().await?;
        let dirs: Vec<PathBuf> = installs
            .iter()
            .map(|i| PathBuf::from(&i.root_path))
            .collect();
        let (watcher, mut changes) = AppsWatcher::start(dirs, debounce)?;

        let task = tokio::spawn(async move {
            if let Err(err) = manager.refresh_and_repair().await {
                tracing::warn!(%err, "initial repair pass failed");
            }
            while changes.recv().await.is_some() {
                match manager.refresh_and_repair().await {
                    Ok(results) => {
                        for (id, result) in results {
                            match result {
                                Ok(report) => {
                                    tracing::info!(install = id, outcome = ?report.outcome, "repair finished")
                                }
                                Err(err) => tracing::warn!(install = id, %err, "repair failed"),
                            }
                        }
                    }
                    Err(err) => tracing::warn!(%err, "repair pass failed"),
                }
            }
        });
        Ok(AutoRepatch {
            task,
            _watcher: watcher,
        })
    }

    pub fn stop(self) {}
}

impl Drop for AutoRepatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::read_patch_info;
    use crate::engine::testutil::{make_spa, INDEX_HTML};
    use crate::fsutil::sha256_file;
    use crate::source::StaticSource;
    use ghostify_core::{AppPaths, Config, EventBus};
    use ghostify_db::repo::installs::InstallKind;
    use ghostify_db::Db;
    use ghostify_spotify::{DetectedInstall, SpotifyVersion, SPA_FILE};
    use std::sync::Arc;
    use std::time::Instant;

    async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    #[tokio::test]
    async fn spotify_update_is_repatched_automatically() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(tmp.path().join("ghostify"));
        paths.ensure().unwrap();
        let apps = tmp.path().join("spotify/Apps");
        std::fs::create_dir_all(&apps).unwrap();
        let spa = apps.join(SPA_FILE);
        make_spa(&spa, &[("index.html", INDEX_HTML), ("xpui.js", "v1")]);

        let found = DetectedInstall {
            kind: InstallKind::Custom,
            apps_dir: apps.clone(),
            spa_path: spa.clone(),
            prefs_file: None,
            version: Some(SpotifyVersion::parse("1.2.31").unwrap()),
            writable: true,
        };
        let detected = found.clone();
        let manager = PatchManager::new(
            Db::open_in_memory().unwrap(),
            &paths,
            Arc::new(Config::default()),
            EventBus::new(32),
            Arc::new(
                StaticSource::builtin("js", "css", &crate::engine::testutil::loader_config())
                    .unwrap(),
            ),
        )
        .with_detector(move || vec![detected.clone()]);

        let id = manager.sync_installs(vec![found]).await.unwrap()[0].id;
        manager.patch(id).await.unwrap();

        let service = AutoRepatch::start(manager.clone(), Duration::from_millis(150))
            .await
            .unwrap();

        // Spotify "updates": the patched bundle is replaced by a fresh original.
        make_spa(&spa, &[("index.html", INDEX_HTML), ("xpui.js", "v2")]);
        let new_original = sha256_file(&spa).unwrap();

        let spa_for_check = spa.clone();
        wait_until("automatic repatch", move || {
            matches!(read_patch_info(&spa_for_check), Ok(Some(info)) if info.original_sha256 == new_original)
        })
        .await;

        // Once repaired, nothing keeps rewriting the file.
        let settled = sha256_file(&spa).unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(sha256_file(&spa).unwrap(), settled);
        service.stop();
    }

    #[tokio::test]
    async fn start_repairs_immediately_without_a_file_change() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(tmp.path().join("ghostify"));
        paths.ensure().unwrap();
        let apps = tmp.path().join("Apps");
        std::fs::create_dir_all(&apps).unwrap();
        let spa = apps.join(SPA_FILE);
        make_spa(&spa, &[("index.html", INDEX_HTML)]);
        let found = DetectedInstall {
            kind: InstallKind::Custom,
            apps_dir: apps,
            spa_path: spa.clone(),
            prefs_file: None,
            version: None,
            writable: true,
        };
        let detected = found.clone();
        let manager = PatchManager::new(
            Db::open_in_memory().unwrap(),
            &paths,
            Arc::new(Config::default()),
            EventBus::new(32),
            Arc::new(
                StaticSource::builtin("js", "css", &crate::engine::testutil::loader_config())
                    .unwrap(),
            ),
        )
        .with_detector(move || vec![detected.clone()]);
        let id = manager.sync_installs(vec![found]).await.unwrap()[0].id;
        manager.patch(id).await.unwrap();
        // Spotify was updated while Ghostify was not running.
        make_spa(
            &spa,
            &[("index.html", INDEX_HTML), ("xpui.js", "offline update")],
        );

        let _service = AutoRepatch::start(manager, Duration::from_millis(150))
            .await
            .unwrap();
        let spa_for_check = spa.clone();
        wait_until("repatch at start-up", move || {
            matches!(read_patch_info(&spa_for_check), Ok(Some(_)))
        })
        .await;
    }
}
