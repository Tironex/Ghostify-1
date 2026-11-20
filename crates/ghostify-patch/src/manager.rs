//! Orchestration of patch, restore and repair for all Spotify installations.
//!
//! The manager ties together detection, the rule source, backups, the engine,
//! the database (state and history) and the event bus. All file work runs on
//! Tokio's blocking pool, and one lock serialises operations so that two
//! patches never touch `xpui.spa` at the same time.

use crate::backup::{BackupEntry, BackupStore};
use crate::engine::{self, Asset, PatchInfo};
use crate::fsutil::sha256_file;
use crate::rules::RuleSet;
use crate::source::PatchSource;
use ghostify_core::{AppPaths, Config, Error, Event, EventBus, Result};
use ghostify_db::repo::installs::{self, SpotifyInstall};
use ghostify_db::repo::patch::{self, PatchAction, PatchOutcome, PatchState, PatchStatus};
use ghostify_db::Db;
use ghostify_spotify::{DetectedInstall, SpotifyVersion, SPA_FILE};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Patch history entries kept per installation.
const HISTORY_LIMIT: u32 = 50;

type Detector = dyn Fn() -> Vec<DetectedInstall> + Send + Sync;

/// What an operation ended up doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchResult {
    /// The bundle was patched with the current rules already; nothing changed.
    AlreadyHealthy,
    Patched,
    /// Spotify had replaced the bundle (an update); it was patched again.
    Repatched,
    /// The bundle was patched with older rules or assets and has been redone.
    Upgraded,
    Restored,
    /// Not patched, so there was nothing to restore.
    NothingToRestore,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PatchReport {
    pub install_id: i64,
    pub outcome: PatchResult,
    pub state: PatchState,
}

/// State of `xpui.spa` on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DiskState {
    Missing,
    Unpatched,
    Patched {
        ruleset_version: String,
        spotify_version: Option<String>,
    },
    Unreadable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusReport {
    pub install: SpotifyInstall,
    pub state: Option<PatchState>,
    pub disk: DiskState,
    pub backups: usize,
}

struct Ctx {
    db: Db,
    store: BackupStore,
    config: Arc<Config>,
    events: EventBus,
    source: Arc<dyn PatchSource>,
}

#[derive(Clone)]
pub struct PatchManager {
    ctx: Arc<Ctx>,
    detector: Arc<Detector>,
    lock: Arc<tokio::sync::Mutex<()>>,
}

enum Health {
    Unpatched,
    Healthy,
    /// Patched, but with other rules/assets than the current ones, or damaged.
    Stale(PatchInfo),
}

impl PatchManager {
    pub fn new(
        db: Db,
        paths: &AppPaths,
        config: Arc<Config>,
        events: EventBus,
        source: Arc<dyn PatchSource>,
    ) -> PatchManager {
        PatchManager {
            ctx: Arc::new(Ctx {
                db,
                store: BackupStore::new(paths.backups_dir()),
                config,
                events,
                source,
            }),
            detector: Arc::new(ghostify_spotify::detect_current),
            lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Replaces how installations are found (the default scans the system).
    pub fn with_detector(
        mut self,
        detector: impl Fn() -> Vec<DetectedInstall> + Send + Sync + 'static,
    ) -> PatchManager {
        self.detector = Arc::new(detector);
        self
    }

    async fn blocking<T, F>(&self, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Ctx) -> Result<T> + Send + 'static,
    {
        let ctx = self.ctx.clone();
        tokio::task::spawn_blocking(move || work(&ctx))
            .await
            .map_err(|e| Error::Internal(format!("patch task failed: {e}")))?
    }

    async fn install(&self, id: i64) -> Result<SpotifyInstall> {
        self.ctx
            .db
            .call(move |c| {
                installs::get(c, id)?.ok_or_else(|| Error::NotFound(format!("installation {id}")))
            })
            .await
    }

    /// Records found installations in the database and announces version changes.
    pub async fn sync_installs(&self, found: Vec<DetectedInstall>) -> Result<Vec<SpotifyInstall>> {
        let (rows, changes) = self
            .ctx
            .db
            .call(move |c| {
                let previous: HashMap<String, Option<String>> = installs::list(c)?
                    .into_iter()
                    .map(|row| (row.root_path, row.version))
                    .collect();
                let mut rows = Vec::new();
                let mut changes = Vec::new();
                for item in &found {
                    let row = installs::upsert(
                        c,
                        item.kind,
                        &item.apps_dir.to_string_lossy(),
                        item.version.as_ref().map(SpotifyVersion::as_str),
                    )?;
                    if let (Some(old), Some(new)) = (previous.get(&row.root_path), &row.version) {
                        if old.as_ref() != Some(new) {
                            changes.push((old.clone(), new.clone()));
                        }
                    }
                    rows.push(row);
                }
                Ok((rows, changes))
            })
            .await?;
        for (previous, current) in changes {
            self.ctx
                .events
                .publish(Event::SpotifyVersionChanged { previous, current });
        }
        Ok(rows)
    }

    /// Scans the system and records what was found.
    pub async fn detect_and_sync(&self) -> Result<Vec<SpotifyInstall>> {
        let detector = self.detector.clone();
        let found = tokio::task::spawn_blocking(move || detector())
            .await
            .map_err(|e| Error::Internal(format!("detection failed: {e}")))?;
        self.sync_installs(found).await
    }

    /// Patches an installation. Idempotent: an up-to-date patch is left alone.
    pub async fn patch(&self, install_id: i64) -> Result<PatchReport> {
        let install = self.install(install_id).await?;
        let _guard = self.lock.lock().await;
        self.blocking(move |ctx| ensure_patched(ctx, &install, PatchAction::Patch))
            .await
    }

    /// Brings an installation back into the patched state if Spotify or an
    /// update of Ghostify's rules has changed the bundle.
    pub async fn repair(&self, install_id: i64) -> Result<PatchReport> {
        let install = self.install(install_id).await?;
        let _guard = self.lock.lock().await;
        self.blocking(move |ctx| ensure_patched(ctx, &install, PatchAction::Repatch))
            .await
    }

    /// Repairs every installation the user has patched. Installations that
    /// are not patched (or whose last patch failed) are left alone, so a
    /// broken rule set does not retry forever.
    pub async fn repair_all(&self) -> Vec<(i64, Result<PatchReport>)> {
        let wanted = self
            .ctx
            .db
            .call(|c| {
                let mut wanted = Vec::new();
                for install in installs::list(c)? {
                    if let Some(state) = patch::get_state(c, install.id)? {
                        if state.status == PatchStatus::Patched {
                            wanted.push(install.id);
                        }
                    }
                }
                Ok(wanted)
            })
            .await
            .unwrap_or_else(|err| {
                tracing::error!(%err, "cannot list installations for repair");
                Vec::new()
            });
        let mut results = Vec::new();
        for id in wanted {
            let result = self.repair(id).await;
            if let Err(err) = &result {
                tracing::warn!(install = id, %err, "automatic repair failed");
            }
            results.push((id, result));
        }
        results
    }

    /// Detects installations, then repairs the patched ones.
    pub async fn refresh_and_repair(&self) -> Result<Vec<(i64, Result<PatchReport>)>> {
        self.detect_and_sync().await?;
        Ok(self.repair_all().await)
    }

    /// Puts Spotify's original bundle back.
    pub async fn restore(&self, install_id: i64) -> Result<PatchReport> {
        let install = self.install(install_id).await?;
        let _guard = self.lock.lock().await;
        self.blocking(move |ctx| restore_original(ctx, &install))
            .await
    }

    pub async fn status(&self, install_id: i64) -> Result<StatusReport> {
        let install = self.install(install_id).await?;
        self.blocking(move |ctx| {
            let spa = spa_path(&install);
            let disk = if !spa.is_file() {
                DiskState::Missing
            } else {
                match engine::read_patch_info(&spa) {
                    Ok(None) => DiskState::Unpatched,
                    Ok(Some(info)) => DiskState::Patched {
                        ruleset_version: info.ruleset_version,
                        spotify_version: info.spotify_version,
                    },
                    Err(err) => DiskState::Unreadable {
                        reason: err.to_string(),
                    },
                }
            };
            let id = install.id;
            let state = ctx.db.with(|c| patch::get_state(c, id))?;
            let backups = ctx.store.list(&backup_key(&install))?.len();
            Ok(StatusReport {
                install,
                state,
                disk,
                backups,
            })
        })
        .await
    }
}

fn spa_path(install: &SpotifyInstall) -> PathBuf {
    Path::new(&install.root_path).join(SPA_FILE)
}

fn backup_key(install: &SpotifyInstall) -> String {
    format!("install-{}", install.id)
}

fn progress(ctx: &Ctx, stage: &str, percent: u8) {
    ctx.events.publish(Event::PatchProgress {
        stage: stage.to_owned(),
        percent,
    });
}

fn parse_version(install: &SpotifyInstall) -> Option<SpotifyVersion> {
    install
        .version
        .as_deref()
        .and_then(|v| SpotifyVersion::parse(v).ok())
}

fn inspect(spa: &Path, ruleset: &RuleSet, assets: &[Asset]) -> Result<Health> {
    Ok(match engine::read_patch_info(spa)? {
        None => Health::Unpatched,
        Some(info) => {
            let current = info.ruleset_version == ruleset.version
                && info.ruleset_sequence == ruleset.sequence
                && engine::verify(spa, &info, assets).is_ok();
            if current {
                Health::Healthy
            } else {
                Health::Stale(info)
            }
        }
    })
}

fn record_failure(ctx: &Ctx, install: &SpotifyInstall, action: PatchAction, err: &Error) {
    let message = err.to_string();
    let version = install.version.clone();
    let recorded = ctx.db.with(|c| {
        patch::record_history(c, install.id, action, PatchOutcome::Failure, Some(&message))?;
        patch::prune_history(c, install.id, HISTORY_LIMIT)?;
        patch::set_state(c, install.id, PatchStatus::Failed, None, version.as_deref())?;
        Ok(())
    });
    if let Err(db_err) = recorded {
        tracing::error!(%db_err, "cannot record patch failure");
    }
    progress(ctx, "failed", 0);
}

fn ensure_patched(ctx: &Ctx, install: &SpotifyInstall, action: PatchAction) -> Result<PatchReport> {
    progress(ctx, "prepare", 5);
    match patch_inner(ctx, install, action) {
        Ok((outcome, ruleset_version)) => {
            let version = install.version.clone();
            let state = ctx.db.with(|c| {
                if outcome == PatchResult::AlreadyHealthy {
                    if let Some(state) = patch::get_state(c, install.id)? {
                        if state.status == PatchStatus::Patched {
                            return Ok(state);
                        }
                    }
                } else {
                    patch::record_history(c, install.id, action, PatchOutcome::Success, None)?;
                    patch::prune_history(c, install.id, HISTORY_LIMIT)?;
                }
                patch::set_state(
                    c,
                    install.id,
                    PatchStatus::Patched,
                    Some(&ruleset_version),
                    version.as_deref(),
                )
            })?;
            progress(ctx, "done", 100);
            Ok(PatchReport {
                install_id: install.id,
                outcome,
                state,
            })
        }
        Err(err) => {
            record_failure(ctx, install, action, &err);
            Err(err)
        }
    }
}

fn patch_inner(
    ctx: &Ctx,
    install: &SpotifyInstall,
    action: PatchAction,
) -> Result<(PatchResult, String)> {
    let spa = spa_path(install);
    if !spa.is_file() {
        return Err(Error::NotFound(format!("{} does not exist", spa.display())));
    }
    let version = parse_version(install);
    let (ruleset, assets) = ctx.source.load()?;
    ruleset.validate()?;
    let key = backup_key(install);

    let (backup, result) = match inspect(&spa, &ruleset, &assets)? {
        Health::Healthy => return Ok((PatchResult::AlreadyHealthy, ruleset.version)),
        Health::Unpatched => {
            progress(ctx, "backup", 20);
            let backup = ctx.store.create(&key, &spa, install.version.as_deref())?;
            let result = if action == PatchAction::Repatch {
                PatchResult::Repatched
            } else {
                PatchResult::Patched
            };
            (backup, result)
        }
        Health::Stale(info) => {
            progress(ctx, "backup", 20);
            let backup = ctx.store.find_by_sha(&key, &info.original_sha256)?.ok_or_else(|| {
                Error::NotFound(
                    "no backup of the original xpui.spa for the current patch; reinstall Spotify to repair"
                        .into(),
                )
            })?;
            ctx.store.restore(&backup, &spa)?;
            (backup, PatchResult::Upgraded)
        }
    };

    progress(ctx, "patch", 50);
    if let Err(err) = engine::apply(&spa, &ruleset, &assets, version.as_ref()) {
        safeguard(ctx, &backup, &spa);
        return Err(err);
    }
    progress(ctx, "verify", 90);

    let keep = ctx.config.patch.backups_to_keep as usize;
    if let Err(err) = ctx.store.prune(&key, keep, Some(&backup.manifest.sha256)) {
        tracing::warn!(%err, "cannot prune old backups");
    }
    Ok((result, ruleset.version))
}

/// After a failed patch the bundle must be the original again. `apply` is
/// atomic, so this normally finds nothing to do; it is the last line of defence.
fn safeguard(ctx: &Ctx, backup: &BackupEntry, spa: &Path) {
    let intact = matches!(engine::read_patch_info(spa), Ok(None))
        && sha256_file(spa).is_ok_and(|sha| sha == backup.manifest.sha256);
    if !intact {
        match ctx.store.restore(backup, spa) {
            Ok(()) => tracing::warn!("restored the original xpui.spa after a failed patch"),
            Err(err) => tracing::error!(%err, "could not restore the original xpui.spa"),
        }
    }
}

fn restore_original(ctx: &Ctx, install: &SpotifyInstall) -> Result<PatchReport> {
    progress(ctx, "restore", 10);
    let outcome = restore_inner(ctx, install);
    match outcome {
        Ok(result) => {
            let version = install.version.clone();
            let state = ctx.db.with(|c| {
                patch::record_history(
                    c,
                    install.id,
                    PatchAction::Restore,
                    PatchOutcome::Success,
                    None,
                )?;
                patch::prune_history(c, install.id, HISTORY_LIMIT)?;
                patch::set_state(
                    c,
                    install.id,
                    PatchStatus::Restored,
                    None,
                    version.as_deref(),
                )
            })?;
            progress(ctx, "done", 100);
            Ok(PatchReport {
                install_id: install.id,
                outcome: result,
                state,
            })
        }
        Err(err) => {
            let message = err.to_string();
            let _ = ctx.db.with(|c| {
                patch::record_history(
                    c,
                    install.id,
                    PatchAction::Restore,
                    PatchOutcome::Failure,
                    Some(&message),
                )?;
                Ok(())
            });
            progress(ctx, "failed", 0);
            Err(err)
        }
    }
}

fn restore_inner(ctx: &Ctx, install: &SpotifyInstall) -> Result<PatchResult> {
    let spa = spa_path(install);
    if !spa.is_file() {
        return Err(Error::NotFound(format!("{} does not exist", spa.display())));
    }
    let Some(info) = engine::read_patch_info(&spa)? else {
        return Ok(PatchResult::NothingToRestore);
    };
    let backup = ctx
        .store
        .find_by_sha(&backup_key(install), &info.original_sha256)?
        .ok_or_else(|| {
            Error::NotFound(
                "no intact backup of the original xpui.spa; reinstall Spotify to restore it".into(),
            )
        })?;
    ctx.store.restore(&backup, &spa)?;
    if engine::read_patch_info(&spa)?.is_some() {
        return Err(Error::Internal(
            "restored file is still marked as patched".into(),
        ));
    }
    Ok(PatchResult::Restored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testutil::{make_spa, read_text, INDEX_HTML};
    use crate::source::StaticSource;
    use ghostify_db::repo::installs::InstallKind;

    struct Fixture {
        _tmp: tempfile::TempDir,
        paths: AppPaths,
        db: Db,
        events: EventBus,
        spa: PathBuf,
        detected: DetectedInstall,
        manager: PatchManager,
        install_id: i64,
    }

    fn original_bundle(spa: &Path, js: &str) {
        make_spa(spa, &[("index.html", INDEX_HTML), ("xpui.js", js)]);
    }

    fn detected(apps: &Path, version: &str) -> DetectedInstall {
        DetectedInstall {
            kind: InstallKind::Custom,
            apps_dir: apps.to_path_buf(),
            spa_path: apps.join(SPA_FILE),
            prefs_file: None,
            version: Some(SpotifyVersion::parse(version).unwrap()),
            writable: true,
        }
    }

    fn manager_for(
        db: &Db,
        paths: &AppPaths,
        events: &EventBus,
        source: StaticSource,
    ) -> PatchManager {
        PatchManager::new(
            db.clone(),
            paths,
            Arc::new(Config::default()),
            events.clone(),
            Arc::new(source),
        )
    }

    async fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(tmp.path().join("ghostify"));
        paths.ensure().unwrap();
        let apps = tmp.path().join("spotify/Apps");
        std::fs::create_dir_all(&apps).unwrap();
        let spa = apps.join(SPA_FILE);
        original_bundle(&spa, "console.log(1);");
        let db = Db::open_in_memory().unwrap();
        let events = EventBus::new(64);
        let manager = manager_for(
            &db,
            &paths,
            &events,
            StaticSource::builtin(
                "/* loader 1 */",
                "/* css */",
                &crate::engine::testutil::loader_config(),
            )
            .unwrap(),
        );
        let detected = detected(&apps, "1.2.31.1205.gabc");
        let rows = manager.sync_installs(vec![detected.clone()]).await.unwrap();
        Fixture {
            _tmp: tmp,
            paths,
            db,
            events,
            spa,
            detected,
            manager,
            install_id: rows[0].id,
        }
    }

    fn history(f: &Fixture) -> Vec<ghostify_db::repo::patch::PatchHistoryEntry> {
        let id = f.install_id;
        f.db.with(|c| patch::history(c, id, 50)).unwrap()
    }

    #[tokio::test]
    async fn patch_then_restore_returns_the_exact_original() {
        let f = fixture().await;
        let original_sha = sha256_file(&f.spa).unwrap();

        let report = f.manager.patch(f.install_id).await.unwrap();
        assert_eq!(report.outcome, PatchResult::Patched);
        assert_eq!(report.state.status, PatchStatus::Patched);
        assert_eq!(
            report.state.applied_rules_version.as_deref(),
            Some("builtin-2")
        );
        assert!(read_text(&f.spa, "index.html").contains("ghostify/loader.js"));

        let status = f.manager.status(f.install_id).await.unwrap();
        assert!(matches!(status.disk, DiskState::Patched { .. }));
        assert_eq!(status.backups, 1);

        let restored = f.manager.restore(f.install_id).await.unwrap();
        assert_eq!(restored.outcome, PatchResult::Restored);
        assert_eq!(restored.state.status, PatchStatus::Restored);
        assert_eq!(
            sha256_file(&f.spa).unwrap(),
            original_sha,
            "restore must be byte exact"
        );
        assert_eq!(
            f.manager.status(f.install_id).await.unwrap().disk,
            DiskState::Unpatched
        );

        let again = f.manager.restore(f.install_id).await.unwrap();
        assert_eq!(again.outcome, PatchResult::NothingToRestore);
    }

    #[tokio::test]
    async fn patching_is_idempotent_and_history_only_records_real_changes() {
        let f = fixture().await;
        f.manager.patch(f.install_id).await.unwrap();
        let sha_after_first = sha256_file(&f.spa).unwrap();
        let second = f.manager.patch(f.install_id).await.unwrap();
        assert_eq!(second.outcome, PatchResult::AlreadyHealthy);
        assert_eq!(
            sha256_file(&f.spa).unwrap(),
            sha_after_first,
            "healthy bundle must not be rewritten"
        );
        assert_eq!(history(&f).len(), 1);
    }

    #[tokio::test]
    async fn spotify_update_is_repatched_with_a_new_backup() {
        let f = fixture().await;
        f.manager.patch(f.install_id).await.unwrap();

        // Spotify updates itself: the patched bundle is replaced by a new original.
        original_bundle(&f.spa, "console.log(2); // new spotify");
        let new_original = sha256_file(&f.spa).unwrap();

        let report = f.manager.repair(f.install_id).await.unwrap();
        assert_eq!(report.outcome, PatchResult::Repatched);
        let info = engine::read_patch_info(&f.spa).unwrap().unwrap();
        assert_eq!(info.original_sha256, new_original);
        assert_eq!(f.manager.status(f.install_id).await.unwrap().backups, 2);
        assert_eq!(history(&f)[0].action, PatchAction::Repatch);

        // Restoring now yields the new Spotify version, not the old one.
        f.manager.restore(f.install_id).await.unwrap();
        assert_eq!(sha256_file(&f.spa).unwrap(), new_original);
    }

    #[tokio::test]
    async fn failed_patch_keeps_the_original_and_records_the_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(tmp.path().join("g"));
        paths.ensure().unwrap();
        let apps = tmp.path().join("Apps");
        std::fs::create_dir_all(&apps).unwrap();
        let spa = apps.join(SPA_FILE);
        make_spa(
            &spa,
            &[("index.html", "<html><body>no head element</body></html>")],
        );
        let before = sha256_file(&spa).unwrap();
        let db = Db::open_in_memory().unwrap();
        let events = EventBus::new(16);
        let manager = manager_for(
            &db,
            &paths,
            &events,
            StaticSource::builtin("js", "css", &crate::engine::testutil::loader_config()).unwrap(),
        );
        let id = manager
            .sync_installs(vec![detected(&apps, "1.2.3")])
            .await
            .unwrap()[0]
            .id;

        let err = manager.patch(id).await.unwrap_err();
        assert_eq!(err.code(), "conflict");
        assert_eq!(sha256_file(&spa).unwrap(), before);

        let status = manager.status(id).await.unwrap();
        assert_eq!(status.state.unwrap().status, PatchStatus::Failed);
        assert_eq!(status.disk, DiskState::Unpatched);
        let entries = db.with(|c| patch::history(c, id, 10)).unwrap();
        assert_eq!(entries[0].outcome, PatchOutcome::Failure);
        assert!(entries[0]
            .error
            .as_deref()
            .unwrap()
            .contains("inject-loader"));
    }

    #[tokio::test]
    async fn newer_rules_or_assets_upgrade_an_existing_patch_from_the_same_original() {
        let f = fixture().await;
        f.manager.patch(f.install_id).await.unwrap();
        let original_sha = engine::read_patch_info(&f.spa)
            .unwrap()
            .unwrap()
            .original_sha256;

        let mut newer = crate::rules::builtin();
        newer.version = "builtin-2".into();
        newer.sequence = 2;
        let source = StaticSource::new(
            newer,
            vec![Asset {
                path: "loader.js".into(),
                bytes: b"/* loader 2 */".to_vec(),
            }],
        );
        let upgraded_manager = manager_for(&f.db, &f.paths, &f.events, source);
        let report = upgraded_manager.repair(f.install_id).await.unwrap();

        assert_eq!(report.outcome, PatchResult::Upgraded);
        let info = engine::read_patch_info(&f.spa).unwrap().unwrap();
        assert_eq!(info.ruleset_version, "builtin-2");
        assert_eq!(
            info.original_sha256, original_sha,
            "upgrade must start from the real original"
        );
        assert_eq!(read_text(&f.spa, "ghostify/loader.js"), "/* loader 2 */");
        assert_eq!(read_text(&f.spa, "xpui.js"), "console.log(1);");
        assert_eq!(
            upgraded_manager.status(f.install_id).await.unwrap().backups,
            1
        );

        // Same rules again: nothing to do.
        assert_eq!(
            upgraded_manager.repair(f.install_id).await.unwrap().outcome,
            PatchResult::AlreadyHealthy
        );
    }

    #[tokio::test]
    async fn damaged_patched_bundle_is_rebuilt_from_the_backup() {
        let f = fixture().await;
        f.manager.patch(f.install_id).await.unwrap();
        let mut bytes = std::fs::read(&f.spa).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xFF;
        std::fs::write(&f.spa, bytes).unwrap();

        // Either the archive no longer opens (error, state Failed) or it is rebuilt.
        match f.manager.repair(f.install_id).await {
            Ok(report) => assert_eq!(report.outcome, PatchResult::Upgraded),
            Err(err) => assert!(matches!(err.code(), "invalid_input" | "io"), "{err}"),
        }
    }

    #[tokio::test]
    async fn repair_all_only_touches_patched_installations() {
        let f = fixture().await;
        // Not patched yet: nothing happens.
        assert!(f.manager.repair_all().await.is_empty());

        f.manager.patch(f.install_id).await.unwrap();
        original_bundle(&f.spa, "console.log('updated');");
        let results = f.manager.repair_all().await;
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].1.as_ref().unwrap().outcome,
            PatchResult::Repatched
        );

        // After a user restore the installation is left alone, even if Spotify changes.
        f.manager.restore(f.install_id).await.unwrap();
        original_bundle(&f.spa, "console.log('updated again');");
        assert!(f.manager.repair_all().await.is_empty());
        assert_eq!(
            f.manager.status(f.install_id).await.unwrap().disk,
            DiskState::Unpatched
        );
    }

    #[tokio::test]
    async fn restore_without_backup_reports_it_and_leaves_the_file() {
        let f = fixture().await;
        f.manager.patch(f.install_id).await.unwrap();
        std::fs::remove_dir_all(f.paths.backups_dir()).unwrap();
        let patched_sha = sha256_file(&f.spa).unwrap();

        let err = f.manager.restore(f.install_id).await.unwrap_err();
        assert_eq!(err.code(), "not_found");
        assert_eq!(sha256_file(&f.spa).unwrap(), patched_sha);
        assert_eq!(history(&f)[0].outcome, PatchOutcome::Failure);
    }

    #[tokio::test]
    async fn unknown_installation_is_not_found() {
        let f = fixture().await;
        assert_eq!(f.manager.patch(999).await.unwrap_err().code(), "not_found");
        assert_eq!(f.manager.status(999).await.unwrap_err().code(), "not_found");
    }

    #[tokio::test]
    async fn progress_and_version_events_are_published() {
        let f = fixture().await;
        let mut rx = f.events.subscribe();
        f.manager.patch(f.install_id).await.unwrap();

        let mut stages = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let Event::PatchProgress { stage, percent } = event {
                stages.push((stage, percent));
            }
        }
        let names: Vec<_> = stages.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(names, ["prepare", "backup", "patch", "verify", "done"]);
        assert!(
            stages.windows(2).all(|w| w[0].1 <= w[1].1),
            "progress must not go backwards"
        );

        let mut newer = f.detected.clone();
        newer.version = Some(SpotifyVersion::parse("1.2.40.1").unwrap());
        f.manager.sync_installs(vec![newer]).await.unwrap();
        match rx.try_recv().unwrap() {
            Event::SpotifyVersionChanged { previous, current } => {
                assert_eq!(previous.as_deref(), Some("1.2.31.1205.gabc"));
                assert_eq!(current, "1.2.40.1");
            }
            other => panic!("unexpected event {other:?}"),
        }
    }

    #[tokio::test]
    async fn detection_feeds_the_database() {
        let f = fixture().await;
        let detected = f.detected.clone();
        let manager = f
            .manager
            .clone()
            .with_detector(move || vec![detected.clone()]);
        let rows = manager.detect_and_sync().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].id, f.install_id,
            "same directory must map to the same installation"
        );
    }
}
