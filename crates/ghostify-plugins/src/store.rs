//! Installed plugins: files, versions, permissions, scan results, crash policy.

use crate::manifest::{Permission, PluginManifest, MANIFEST_FILE, MAX_ENTRY_BYTES};
use crate::package;
use crate::scanner::{self, ScanReport};
use ghostify_core::{AppPaths, Config, Error, Event, EventBus, PluginState, Result};
use ghostify_db::repo::misc::{self, ScanResult, ScanVerdict};
pub use ghostify_db::repo::plugins::PluginSource;
use ghostify_db::repo::plugins::{self, NewPlugin, Plugin, PluginEventKind, PluginRuntime};
use ghostify_db::Db;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);
const SNAPSHOTS_TO_KEEP: u32 = 3;
pub const MAX_STORAGE_VALUE_BYTES: usize = 256 * 1024;
pub const MAX_STORAGE_KEYS: i64 = 500;

#[derive(Debug, Clone, Copy)]
pub struct InstallOptions {
    pub source: PluginSource,
    /// Update an installed plugin instead of failing with a conflict.
    pub replace: bool,
    /// Install although the scan blocked it. Only for an explicit decision of the user.
    pub allow_blocked: bool,
}

impl InstallOptions {
    pub fn new(source: PluginSource) -> InstallOptions {
        InstallOptions {
            source,
            replace: false,
            allow_blocked: false,
        }
    }

    pub fn replacing(mut self) -> InstallOptions {
        self.replace = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginInfo {
    #[serde(flatten)]
    pub plugin: Plugin,
    /// `None` if the files on disk are missing or damaged.
    pub manifest: Option<PluginManifest>,
    pub requested: Vec<String>,
    pub granted: Vec<String>,
    pub scan: Option<ScanResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub info: PluginInfo,
    pub report: ScanReport,
    pub notes: Vec<String>,
}

/// Result of [`PluginStore::inspect_zip`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    pub manifest: PluginManifest,
    pub sha256: String,
    pub report: ScanReport,
}

/// What a running host needs to start one plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoadedPlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub runtime: PluginRuntime,
    pub sha256: String,
    pub permissions: Vec<String>,
    pub hosts: Vec<String>,
    pub code: String,
}

/// Entry of the list of plugins to start (without code).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnabledPlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub runtime: PluginRuntime,
    pub permissions: Vec<String>,
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CrashOutcome {
    pub crashes_in_window: u32,
    pub disabled: bool,
    /// Version restored from a snapshot, if the policy rolled back.
    pub rolled_back_to: Option<String>,
}

#[derive(Clone)]
pub struct PluginStore {
    db: Db,
    config: Arc<Config>,
    root: PathBuf,
    snapshots: PathBuf,
    staging: PathBuf,
    events: Option<EventBus>,
}

struct Prepared {
    manifest: PluginManifest,
    hash: String,
    report: ScanReport,
}

impl PluginStore {
    pub fn new(db: Db, paths: &AppPaths, config: Arc<Config>) -> Result<PluginStore> {
        let store = PluginStore {
            db,
            config,
            root: paths.plugins_dir(),
            snapshots: paths.snapshots_dir(),
            staging: paths.staging_dir().join("plugins"),
            events: None,
        };
        for dir in [&store.root, &store.snapshots, &store.staging] {
            fs::create_dir_all(dir)?;
        }
        Ok(store)
    }

    pub fn with_events(mut self, events: EventBus) -> PluginStore {
        self.events = Some(events);
        self
    }

    fn publish(&self, event: Event) {
        if let Some(bus) = &self.events {
            bus.publish(event);
        }
    }

    /// Tells listeners (the renderer host, the UI) that a plugin changed.
    fn changed(&self, id: &str, state: PluginState) {
        self.publish(Event::PluginState {
            plugin_id: id.to_owned(),
            state,
        });
    }

    pub fn plugin_dir(&self, id: &str) -> Result<PathBuf> {
        crate::manifest::validate_id(id)?;
        Ok(self.root.join(id.replace('/', "__")))
    }

    // ---- installing ----------------------------------------------------

    pub fn install_dir(&self, src: &Path, options: InstallOptions) -> Result<Installed> {
        let stage = Staging::new(&self.staging)?;
        let notes = package::copy_dir(src, stage.path())?;
        self.finish(stage, notes, options)
    }

    pub fn install_zip(&self, zip: &Path, options: InstallOptions) -> Result<Installed> {
        let stage = Staging::new(&self.staging)?;
        let notes = package::extract_zip(zip, stage.path())?;
        self.finish(stage, notes, options)
    }

    /// Reads and scans a plugin archive without installing anything.
    pub fn inspect_zip(&self, zip: &Path) -> Result<Inspection> {
        let stage = Staging::new(&self.staging)?;
        package::extract_zip(zip, stage.path())?;
        let Prepared {
            manifest,
            hash,
            report,
        } = self.prepare(stage.path())?;
        Ok(Inspection {
            manifest,
            sha256: hash,
            report,
        })
    }

    fn prepare(&self, dir: &Path) -> Result<Prepared> {
        let manifest_path = dir.join(MANIFEST_FILE);
        if !manifest_path.is_file() {
            return Err(Error::Invalid("the plugin has no plugin.json".into()));
        }
        let manifest = PluginManifest::parse(&fs::read(manifest_path)?)?;
        if manifest.runtime == PluginRuntime::Legacy && !self.config.plugins.allow_legacy_runtime {
            return Err(Error::PermissionDenied(
                "this plugin needs the legacy runtime, which is switched off in the settings"
                    .into(),
            ));
        }
        let files = package::read_files(dir)?;
        let entry = files
            .iter()
            .find(|(path, _)| *path == manifest.entry)
            .ok_or_else(|| {
                Error::Invalid(format!("the entry file `{}` is missing", manifest.entry))
            })?;
        if entry.1.len() as u64 > MAX_ENTRY_BYTES {
            return Err(Error::Invalid(format!(
                "the entry file is larger than {MAX_ENTRY_BYTES} bytes"
            )));
        }
        Ok(Prepared {
            hash: package::content_hash(&files),
            report: scanner::scan(&files),
            manifest,
        })
    }

    fn finish(
        &self,
        stage: Staging,
        notes: Vec<String>,
        options: InstallOptions,
    ) -> Result<Installed> {
        let prepared = self.prepare(stage.path())?;
        let Prepared {
            manifest,
            hash,
            report,
        } = prepared;
        let id = manifest.id.clone();
        if report.verdict == ScanVerdict::Blocked && !options.allow_blocked {
            let reasons: Vec<String> = report
                .findings
                .iter()
                .filter(|f| f.severity == scanner::Severity::Block)
                .take(5)
                .map(|f| format!("{} ({}:{})", f.message, f.file, f.line))
                .collect();
            return Err(Error::PermissionDenied(format!(
                "the security scan blocked `{id}`: {}",
                reasons.join("; ")
            )));
        }
        let existing = self.db.with(|c| plugins::get(c, &id))?;
        if existing.is_some() && !options.replace {
            return Err(Error::Conflict(format!(
                "plugin `{id}` is already installed"
            )));
        }
        let target = self.plugin_dir(&id)?;
        if existing.is_none() && target.exists() {
            fs::remove_dir_all(&target)?; // left over from an interrupted install
        }

        // Keep the version that is replaced, so a crash can roll back to it.
        if let Some(old) = &existing {
            if target.is_dir() {
                self.snapshot(old, &target)?;
            }
        }
        let backup = target.with_extension("old");
        let had_files = target.is_dir();
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

        let requested: Vec<&str> = manifest
            .permissions
            .iter()
            .map(Permission::as_str)
            .collect();
        let result = self.db.with(|c| {
            if existing.is_some() {
                plugins::set_version(c, &id, &manifest.version, Some(&hash))?;
                // An update never grants itself more: grants it no longer asks for are dropped,
                // new permissions have to be granted again.
                for granted in plugins::permissions(c, &id)? {
                    if !requested.contains(&granted.as_str()) {
                        plugins::revoke(c, &id, &granted)?;
                    }
                }
                plugins::record_event(c, &id, PluginEventKind::Updated, Some(&manifest.version))?;
            } else {
                plugins::install(
                    c,
                    &NewPlugin {
                        id: id.clone(),
                        name: manifest.name.clone(),
                        version: manifest.version.clone(),
                        source: options.source,
                        runtime: manifest.runtime,
                        sha256: Some(hash.clone()),
                    },
                )?;
            }
            let findings: Vec<String> = report
                .findings
                .iter()
                .map(scanner::Finding::summary)
                .collect();
            misc::record_scan(c, &id, &hash, report.verdict, &findings)?;
            Ok(())
        });
        if let Err(err) = result {
            let _ = fs::remove_dir_all(&target);
            if had_files {
                let _ = fs::rename(&backup, &target);
            }
            return Err(err);
        }
        let _ = fs::remove_dir_all(&backup);
        let info = self.info(&id)?;
        self.changed(&id, info.plugin.state);
        Ok(Installed {
            info,
            report,
            notes,
        })
    }

    fn snapshot(&self, plugin: &Plugin, dir: &Path) -> Result<()> {
        let safe_version: String = plugin
            .version
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = self.snapshots.join(format!(
            "{}__{}.zip",
            plugin.id.replace('/', "__"),
            safe_version
        ));
        package::write_zip(dir, &path)?;
        let sha = package::file_sha256(&path)?;
        let id = plugin.id.clone();
        let version = plugin.version.clone();
        let path_text = path.to_string_lossy().into_owned();
        let stale = self.db.with(|c| {
            plugins::add_snapshot(c, &id, &version, &path_text, &sha)?;
            plugins::prune_snapshots(c, &id, SNAPSHOTS_TO_KEEP)
        })?;
        for old in stale {
            let _ = fs::remove_file(old);
        }
        Ok(())
    }

    // ---- reading -------------------------------------------------------

    pub fn list(&self) -> Result<Vec<PluginInfo>> {
        let rows = self.db.with(plugins::list)?;
        rows.into_iter()
            .map(|plugin| self.describe(plugin))
            .collect()
    }

    pub fn info(&self, id: &str) -> Result<PluginInfo> {
        let plugin = self
            .db
            .with(|c| plugins::get(c, id))?
            .ok_or_else(|| Error::NotFound(format!("plugin `{id}`")))?;
        self.describe(plugin)
    }

    fn manifest(&self, id: &str) -> Result<PluginManifest> {
        let path = self.plugin_dir(id)?.join(MANIFEST_FILE);
        let bytes =
            fs::read(&path).map_err(|_| Error::NotFound(format!("files of plugin `{id}`")))?;
        PluginManifest::parse(&bytes)
    }

    fn describe(&self, plugin: Plugin) -> Result<PluginInfo> {
        let manifest = self.manifest(&plugin.id).ok();
        let (granted, scan) = {
            let id = plugin.id.clone();
            let sha = plugin.sha256.clone();
            self.db.with(|c| {
                let granted = plugins::permissions(c, &id)?;
                let scan = match &sha {
                    Some(sha) => misc::latest_scan(c, &id, sha)?,
                    None => None,
                };
                Ok((granted, scan))
            })?
        };
        let requested = manifest
            .as_ref()
            .map(|m| {
                m.permissions
                    .iter()
                    .map(|p| p.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        Ok(PluginInfo {
            plugin,
            manifest,
            requested,
            granted,
            scan,
        })
    }

    // ---- permissions and state -----------------------------------------

    pub fn grant(&self, id: &str, permission: Permission) -> Result<()> {
        let manifest = self.manifest(id)?;
        if !manifest.permissions.contains(&permission) {
            return Err(Error::Invalid(format!(
                "`{id}` does not request the permission `{}`",
                permission.as_str()
            )));
        }
        self.db
            .with(|c| plugins::grant(c, id, permission.as_str()))?;
        self.changed(id, self.info(id)?.plugin.state);
        Ok(())
    }

    pub fn revoke(&self, id: &str, permission: Permission) -> Result<bool> {
        let revoked = self
            .db
            .with(|c| plugins::revoke(c, id, permission.as_str()))?;
        if revoked {
            self.changed(id, self.info(id)?.plugin.state);
        }
        Ok(revoked)
    }

    pub fn enable(&self, id: &str) -> Result<PluginInfo> {
        let info = self.info(id)?;
        let manifest = info
            .manifest
            .as_ref()
            .ok_or_else(|| Error::Invalid(format!("the files of `{id}` are missing or damaged")))?;
        if manifest.runtime == PluginRuntime::Legacy && !self.config.plugins.allow_legacy_runtime {
            return Err(Error::PermissionDenied(
                "the legacy runtime is switched off in the settings".into(),
            ));
        }
        if info
            .scan
            .as_ref()
            .is_some_and(|s| s.verdict == ScanVerdict::Blocked)
        {
            return Err(Error::PermissionDenied(format!(
                "`{id}` was blocked by the security scan"
            )));
        }
        self.db.with(|c| {
            plugins::set_enabled(c, id, true)?;
            plugins::set_state(c, id, PluginState::Loaded)?;
            plugins::record_event(c, id, PluginEventKind::Activated, None)
        })?;
        self.changed(id, PluginState::Loaded);
        self.info(id)
    }

    pub fn disable(&self, id: &str) -> Result<PluginInfo> {
        self.db.with(|c| {
            plugins::set_enabled(c, id, false)?;
            plugins::set_state(c, id, PluginState::Unloaded)?;
            plugins::record_event(c, id, PluginEventKind::Deactivated, None)
        })?;
        self.publish(Event::PluginState {
            plugin_id: id.to_owned(),
            state: PluginState::Unloaded,
        });
        self.info(id)
    }

    /// The host reports lifecycle changes it observes.
    pub fn report_state(&self, id: &str, state: PluginState) -> Result<()> {
        if !matches!(
            state,
            PluginState::Loaded | PluginState::Active | PluginState::Suspended
        ) {
            return Err(Error::Invalid(
                "a host may only report loaded, active or suspended".into(),
            ));
        }
        let plugin = self
            .db
            .with(|c| plugins::get(c, id))?
            .ok_or_else(|| Error::NotFound(format!("plugin `{id}`")))?;
        if !plugin.enabled {
            return Err(Error::PermissionDenied(format!(
                "plugin `{id}` is disabled"
            )));
        }
        self.db.with(|c| plugins::set_state(c, id, state))?;
        self.publish(Event::PluginState {
            plugin_id: id.to_owned(),
            state,
        });
        Ok(())
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let dir = self.plugin_dir(id)?;
        let paths = self.db.with(|c| plugins::remove(c, id))?;
        for path in paths {
            let _ = fs::remove_file(path);
        }
        if dir.exists() {
            fs::remove_dir_all(dir)?;
        }
        let pattern = format!("{}%", like_escape(&format!("plugin-data:{id}:")));
        self.db.with(|c| {
            c.execute(
                "DELETE FROM settings WHERE key LIKE ?1 ESCAPE '\\'",
                [pattern],
            )
            .map_err(ghostify_db::map_err)?;
            Ok(())
        })?;
        self.publish(Event::PluginState {
            plugin_id: id.to_owned(),
            state: PluginState::Removed,
        });
        Ok(())
    }

    // ---- running -------------------------------------------------------

    /// Enabled plugins and the permissions they actually hold.
    pub fn enabled(&self) -> Result<Vec<EnabledPlugin>> {
        let mut out = Vec::new();
        for info in self.list()?.into_iter().filter(|i| i.plugin.enabled) {
            let Some(manifest) = info.manifest else {
                tracing::warn!(plugin = %info.plugin.id, "enabled plugin has damaged files, skipped");
                continue;
            };
            out.push(EnabledPlugin {
                id: info.plugin.id,
                name: info.plugin.name,
                version: info.plugin.version,
                runtime: info.plugin.runtime,
                permissions: info.granted,
                hosts: manifest.hosts,
            });
        }
        Ok(out)
    }

    /// The code of an enabled plugin. Verifies the files against the installed hash first.
    pub fn load(&self, id: &str) -> Result<LoadedPlugin> {
        let info = self.info(id)?;
        if !info.plugin.enabled {
            return Err(Error::PermissionDenied(format!(
                "plugin `{id}` is disabled"
            )));
        }
        let manifest = info
            .manifest
            .ok_or_else(|| Error::Invalid(format!("the files of `{id}` are missing or damaged")))?;
        let files = package::read_files(&self.plugin_dir(id)?)?;
        let hash = package::content_hash(&files);
        if info.plugin.sha256.as_deref() != Some(hash.as_str()) {
            self.db.with(|c| {
                plugins::set_enabled(c, id, false)?;
                plugins::record_event(
                    c,
                    id,
                    PluginEventKind::Disabled,
                    Some("files were modified after installation"),
                )
            })?;
            self.publish(Event::PluginState {
                plugin_id: id.to_owned(),
                state: PluginState::Unloaded,
            });
            return Err(Error::Conflict(format!("the files of `{id}` were modified after installation; the plugin has been disabled")));
        }
        let entry = files
            .into_iter()
            .find(|(path, _)| *path == manifest.entry)
            .ok_or_else(|| Error::NotFound("entry file".into()))?;
        let code = String::from_utf8(entry.1)
            .map_err(|_| Error::Invalid("the entry file is not UTF-8".into()))?;
        Ok(LoadedPlugin {
            id: info.plugin.id,
            name: info.plugin.name,
            version: info.plugin.version,
            runtime: info.plugin.runtime,
            sha256: hash,
            permissions: info.granted,
            hosts: manifest.hosts,
            code,
        })
    }

    /// Checks that `id` is enabled and holds `permission` (for the network: for `host`).
    pub fn authorize(&self, id: &str, permission: Permission, host: Option<&str>) -> Result<()> {
        let plugin = self
            .db
            .with(|c| plugins::get(c, id))?
            .ok_or_else(|| Error::NotFound(format!("plugin `{id}`")))?;
        if !plugin.enabled {
            return Err(Error::PermissionDenied(format!(
                "plugin `{id}` is disabled"
            )));
        }
        if !self
            .db
            .with(|c| plugins::has_permission(c, id, permission.as_str()))?
        {
            return Err(Error::PermissionDenied(format!(
                "plugin `{id}` does not hold the permission `{}`",
                permission.as_str()
            )));
        }
        if permission == Permission::Network {
            let host = host.ok_or_else(|| {
                Error::Invalid("a host is required for the network permission".into())
            })?;
            if !self.manifest(id)?.allows_host(host) {
                return Err(Error::PermissionDenied(format!(
                    "plugin `{id}` may not contact `{host}`"
                )));
            }
        }
        Ok(())
    }

    // ---- storage -------------------------------------------------------

    fn storage_key(id: &str, key: &str) -> Result<String> {
        let ok = !key.is_empty()
            && key.len() <= 128
            && key.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/' | '@')
            });
        if !ok {
            return Err(Error::Invalid(
                "storage keys are 1-128 characters of letters, digits and . _ - : / @".into(),
            ));
        }
        Ok(format!("plugin-data:{id}:{key}"))
    }

    pub fn storage_get(&self, id: &str, key: &str) -> Result<Option<serde_json::Value>> {
        self.authorize(id, Permission::Storage, None)?;
        let key = Self::storage_key(id, key)?;
        self.db.with(|c| ghostify_db::repo::settings::get(c, &key))
    }

    pub fn storage_set(&self, id: &str, key: &str, value: &serde_json::Value) -> Result<()> {
        self.authorize(id, Permission::Storage, None)?;
        let key = Self::storage_key(id, key)?;
        if serde_json::to_vec(value)?.len() > MAX_STORAGE_VALUE_BYTES {
            return Err(Error::Invalid(format!(
                "a stored value may have at most {MAX_STORAGE_VALUE_BYTES} bytes"
            )));
        }
        let pattern = format!("{}%", like_escape(&format!("plugin-data:{id}:")));
        self.db.with(|c| {
            let count: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM settings WHERE key LIKE ?1 ESCAPE '\\'",
                    [&pattern],
                    |r| r.get(0),
                )
                .map_err(ghostify_db::map_err)?;
            let exists = ghostify_db::repo::settings::get::<serde_json::Value>(c, &key)?.is_some();
            if !exists && count >= MAX_STORAGE_KEYS {
                return Err(Error::Invalid(format!(
                    "a plugin may store at most {MAX_STORAGE_KEYS} keys"
                )));
            }
            ghostify_db::repo::settings::set(c, &key, value)
        })
    }

    pub fn storage_delete(&self, id: &str, key: &str) -> Result<bool> {
        self.authorize(id, Permission::Storage, None)?;
        let key = Self::storage_key(id, key)?;
        self.db
            .with(|c| ghostify_db::repo::settings::delete(c, &key))
    }

    // ---- crashes -------------------------------------------------------

    /// Applies the crash policy: after `crash_threshold` crashes within
    /// `crash_window_secs` the plugin is disabled and, if an older version was
    /// kept, rolled back to it (still disabled, the user decides about re-enabling).
    pub fn report_crash(
        &self,
        id: &str,
        kind: PluginEventKind,
        detail: &str,
    ) -> Result<CrashOutcome> {
        let detail: String = detail
            .chars()
            .filter(|c| !c.is_control())
            .take(500)
            .collect();
        let (threshold, window) = (
            self.config.plugins.crash_threshold,
            self.config.plugins.crash_window_secs,
        );
        let verdict = self
            .db
            .with(|c| plugins::record_crash(c, id, kind, Some(&detail), threshold, window))?;
        let mut rolled_back_to = None;
        if let Some(snapshot) = &verdict.rollback {
            match self.restore_snapshot(id, snapshot) {
                Ok(version) => rolled_back_to = Some(version),
                Err(err) => {
                    tracing::warn!(plugin = id, %err, "rollback failed, plugin stays disabled")
                }
            }
        }
        self.publish(Event::PluginCrash {
            plugin_id: id.to_owned(),
            reason: detail,
            rolled_back: rolled_back_to.is_some(),
        });
        if verdict.disabled {
            self.publish(Event::PluginState {
                plugin_id: id.to_owned(),
                state: PluginState::Crashed,
            });
        }
        Ok(CrashOutcome {
            crashes_in_window: verdict.crashes_in_window,
            disabled: verdict.disabled,
            rolled_back_to,
        })
    }

    fn restore_snapshot(&self, id: &str, snapshot: &plugins::Snapshot) -> Result<String> {
        let path = Path::new(&snapshot.path);
        if package::file_sha256(path)? != snapshot.sha256 {
            return Err(Error::Conflict(
                "the snapshot file does not match its recorded hash".into(),
            ));
        }
        let stage = Staging::new(&self.staging)?;
        package::extract_zip(path, stage.path())?;
        let prepared = self.prepare(stage.path())?;
        if prepared.manifest.id != id {
            return Err(Error::Invalid(
                "the snapshot belongs to a different plugin".into(),
            ));
        }
        let target = self.plugin_dir(id)?;
        let backup = target.with_extension("old");
        let _ = fs::remove_dir_all(&backup);
        if target.exists() {
            fs::rename(&target, &backup)?;
        }
        if let Err(err) = fs::rename(stage.path(), &target) {
            let _ = fs::rename(&backup, &target);
            return Err(err.into());
        }
        stage.disarm();
        let version = prepared.manifest.version.clone();
        let (hash, verdict) = (prepared.hash.clone(), prepared.report.verdict);
        let findings: Vec<String> = prepared
            .report
            .findings
            .iter()
            .map(scanner::Finding::summary)
            .collect();
        let result = self.db.with(|c| {
            plugins::set_version(c, id, &version, Some(&hash))?;
            misc::record_scan(c, id, &hash, verdict, &findings)?;
            plugins::record_event(c, id, PluginEventKind::Rollback, Some(&version))
        });
        if let Err(err) = result {
            let _ = fs::remove_dir_all(&target);
            let _ = fs::rename(&backup, &target);
            return Err(err);
        }
        let _ = fs::remove_dir_all(&backup);
        Ok(version)
    }
}

fn like_escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
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

    pub fn store_with(edit: impl FnOnce(&mut Config)) -> (PluginStore, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(tmp.path());
        let mut config = Config::default();
        edit(&mut config);
        let store =
            PluginStore::new(Db::open_in_memory().unwrap(), &paths, Arc::new(config)).unwrap();
        (store, tmp)
    }

    pub fn store() -> (PluginStore, tempfile::TempDir) {
        store_with(|_| {})
    }

    /// Writes a plugin folder. `extra` is raw JSON appended to the manifest.
    pub fn write_plugin(dir: &Path, id: &str, version: &str, extra: &str, code: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("plugin.json"),
            format!(r#"{{"id":"{id}","name":"Plugin {id}","version":"{version}"{extra}}}"#),
        )
        .unwrap();
        fs::write(dir.join("index.js"), code).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    fn opts() -> InstallOptions {
        InstallOptions::new(PluginSource::Local)
    }

    fn install(
        store: &PluginStore,
        tmp: &Path,
        id: &str,
        version: &str,
        extra: &str,
        code: &str,
        options: InstallOptions,
    ) -> Result<Installed> {
        let src = tmp.join(format!("src-{id}-{version}").replace('/', "_"));
        write_plugin(&src, id, version, extra, code);
        store.install_dir(&src, options)
    }

    #[test]
    fn inspect_reads_an_archive_without_installing() {
        let (store, tmp) = store();
        let src = tmp.path().join("src");
        write_plugin(
            &src,
            "me/p",
            "1.2.3",
            r#","permissions":["storage"]"#,
            "eval(1)",
        );
        let zip = tmp.path().join("p.zip");
        package::write_zip(&src, &zip).unwrap();
        let inspection = store.inspect_zip(&zip).unwrap();
        assert_eq!(inspection.manifest.id, "me/p");
        assert_eq!(inspection.manifest.version, "1.2.3");
        assert_eq!(inspection.report.verdict, ScanVerdict::Blocked);
        assert_eq!(inspection.sha256.len(), 64);
        assert!(store.list().unwrap().is_empty());
        assert!(!store.plugin_dir("me/p").unwrap().exists());
        assert_eq!(fs::read_dir(&store.staging).unwrap().count(), 0);
    }

    #[test]
    fn install_starts_disabled_without_permissions() {
        let (store, tmp) = store();
        let done = install(
            &store,
            tmp.path(),
            "me/p",
            "1.0.0",
            r#","permissions":["storage","ui"]"#,
            "ghostify.log('hi')",
            opts(),
        )
        .unwrap();
        let info = done.info;
        assert!(!info.plugin.enabled && info.granted.is_empty());
        assert_eq!(info.requested, ["storage", "ui"]);
        assert_eq!(info.scan.unwrap().verdict, ScanVerdict::Clean);
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(
            install(&store, tmp.path(), "me/p", "1.0.0", "", "x", opts())
                .unwrap_err()
                .code(),
            "conflict"
        );
    }

    #[test]
    fn blocked_code_is_refused_unless_explicitly_allowed() {
        let (store, tmp) = store();
        let err = install(
            &store,
            tmp.path(),
            "me/evil",
            "1",
            "",
            "eval(atob('...'))",
            opts(),
        )
        .unwrap_err();
        assert_eq!(err.code(), "permission_denied");
        assert!(err.to_string().contains("eval") && store.list().unwrap().is_empty());
        assert!(!store.plugin_dir("me/evil").unwrap().exists());

        let forced = InstallOptions {
            allow_blocked: true,
            ..opts()
        };
        let done = install(&store, tmp.path(), "me/evil", "1", "", "eval(1)", forced).unwrap();
        assert_eq!(done.report.verdict, ScanVerdict::Blocked);
        assert_eq!(
            store.enable("me/evil").unwrap_err().code(),
            "permission_denied",
            "a blocked plugin cannot be enabled"
        );
    }

    #[test]
    fn legacy_runtime_needs_the_setting() {
        let (store, tmp) = store();
        let err = install(
            &store,
            tmp.path(),
            "me/old",
            "1",
            r#","runtime":"legacy""#,
            "x()",
            opts(),
        )
        .unwrap_err();
        assert_eq!(err.code(), "permission_denied");
        let (store, tmp) = store_with(|c| c.plugins.allow_legacy_runtime = true);
        install(
            &store,
            tmp.path(),
            "me/old",
            "1",
            r#","runtime":"legacy""#,
            "x()",
            opts(),
        )
        .unwrap();
        store.enable("me/old").unwrap();
    }

    #[test]
    fn invalid_packages_install_nothing() {
        let (store, tmp) = store();
        let src = tmp.path().join("nomanifest");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("index.js"), "x").unwrap();
        assert!(store.install_dir(&src, opts()).is_err());
        let src = tmp.path().join("noentry");
        write_plugin(&src, "me/x", "1", "", "x");
        fs::remove_file(src.join("index.js")).unwrap();
        assert!(store
            .install_dir(&src, opts())
            .unwrap_err()
            .to_string()
            .contains("entry"));
        assert!(store.list().unwrap().is_empty());
        assert_eq!(fs::read_dir(&store.staging).unwrap().count(), 0);
    }

    #[test]
    fn grants_must_be_requested_and_enable_is_separate() {
        let (store, tmp) = store();
        install(
            &store,
            tmp.path(),
            "me/p",
            "1",
            r#","permissions":["storage"]"#,
            "x()",
            opts(),
        )
        .unwrap();
        assert_eq!(
            store.grant("me/p", Permission::Ui).unwrap_err().code(),
            "invalid_input"
        );
        assert!(store.grant("ghost", Permission::Storage).is_err());
        store.grant("me/p", Permission::Storage).unwrap();
        let info = store.enable("me/p").unwrap();
        assert!(info.plugin.enabled);
        assert_eq!(info.granted, ["storage"]);
        assert!(store.revoke("me/p", Permission::Storage).unwrap());
        assert!(!store.revoke("me/p", Permission::Storage).unwrap());
    }

    #[test]
    fn authorize_checks_enabled_permission_and_hosts() {
        let (store, tmp) = store();
        install(
            &store,
            tmp.path(),
            "me/p",
            "1",
            r#","permissions":["network","storage"],"hosts":["api.example.com"]"#,
            "x()",
            opts(),
        )
        .unwrap();
        let denied = |r: Result<()>| r.unwrap_err().code() == "permission_denied";
        assert!(
            denied(store.authorize("me/p", Permission::Storage, None)),
            "disabled"
        );
        store.enable("me/p").unwrap();
        assert!(
            denied(store.authorize("me/p", Permission::Storage, None)),
            "not granted"
        );
        store.grant("me/p", Permission::Storage).unwrap();
        store.authorize("me/p", Permission::Storage, None).unwrap();
        store.grant("me/p", Permission::Network).unwrap();
        store
            .authorize("me/p", Permission::Network, Some("api.example.com"))
            .unwrap();
        assert!(denied(store.authorize(
            "me/p",
            Permission::Network,
            Some("evil.com")
        )));
        assert_eq!(
            store
                .authorize("me/p", Permission::Network, None)
                .unwrap_err()
                .code(),
            "invalid_input"
        );
        assert_eq!(
            store
                .authorize("ghost", Permission::Ui, None)
                .unwrap_err()
                .code(),
            "not_found"
        );
    }

    #[test]
    fn load_returns_code_and_detects_tampering() {
        let (store, tmp) = store();
        install(&store, tmp.path(), "me/p", "1", "", "const a = 1;", opts()).unwrap();
        assert_eq!(
            store.load("me/p").unwrap_err().code(),
            "permission_denied",
            "disabled"
        );
        store.enable("me/p").unwrap();
        let loaded = store.load("me/p").unwrap();
        assert_eq!(loaded.code, "const a = 1;");
        assert_eq!(loaded.sha256.len(), 64);
        assert_eq!(store.enabled().unwrap().len(), 1);

        fs::write(store.plugin_dir("me/p").unwrap().join("index.js"), "evil()").unwrap();
        assert_eq!(store.load("me/p").unwrap_err().code(), "conflict");
        assert!(
            !store.info("me/p").unwrap().plugin.enabled,
            "tampered plugins are switched off"
        );
        assert!(store.enabled().unwrap().is_empty());
    }

    #[test]
    fn updates_snapshot_the_old_version_and_never_escalate_permissions() {
        let (store, tmp) = store();
        install(
            &store,
            tmp.path(),
            "me/p",
            "1.0.0",
            r#","permissions":["storage","ui"]"#,
            "v1()",
            opts(),
        )
        .unwrap();
        store.grant("me/p", Permission::Storage).unwrap();
        store.grant("me/p", Permission::Ui).unwrap();
        store.enable("me/p").unwrap();

        let done = install(
            &store,
            tmp.path(),
            "me/p",
            "1.1.0",
            r#","permissions":["storage","clipboard"]"#,
            "v2()",
            opts().replacing(),
        )
        .unwrap();
        assert_eq!(done.info.plugin.version, "1.1.0");
        assert!(done.info.plugin.enabled);
        assert_eq!(
            done.info.granted,
            ["storage"],
            "ui dropped, clipboard not granted"
        );
        assert_eq!(store.load("me/p").unwrap().code, "v2()");
        let snaps = store.db.with(|c| plugins::snapshots(c, "me/p")).unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].version, "1.0.0");
        assert!(Path::new(&snaps[0].path).is_file());
    }

    #[test]
    fn crash_policy_disables_and_rolls_back() {
        let (store, tmp) = store_with(|c| {
            c.plugins.crash_threshold = 3;
        });
        let bus = EventBus::new(32);
        let store = store.with_events(bus.clone());
        let mut events = bus.subscribe();
        install(&store, tmp.path(), "me/p", "1.0.0", "", "good()", opts()).unwrap();
        install(
            &store,
            tmp.path(),
            "me/p",
            "2.0.0",
            "",
            "bad()",
            opts().replacing(),
        )
        .unwrap();
        store.enable("me/p").unwrap();

        for expected in 1..=2 {
            let outcome = store
                .report_crash("me/p", PluginEventKind::Crash, "TypeError")
                .unwrap();
            assert_eq!(
                (
                    outcome.crashes_in_window,
                    outcome.disabled,
                    outcome.rolled_back_to
                ),
                (expected, false, None)
            );
        }
        let outcome = store
            .report_crash("me/p", PluginEventKind::Timeout, "hook took too long")
            .unwrap();
        assert!(outcome.disabled);
        assert_eq!(outcome.rolled_back_to.as_deref(), Some("1.0.0"));

        let info = store.info("me/p").unwrap();
        assert_eq!(info.plugin.version, "1.0.0");
        assert!(!info.plugin.enabled);
        assert_eq!(
            fs::read_to_string(store.plugin_dir("me/p").unwrap().join("index.js")).unwrap(),
            "good()"
        );
        // The restored version can be enabled and passes the integrity check.
        store.enable("me/p").unwrap();
        assert_eq!(store.load("me/p").unwrap().code, "good()");

        let mut saw_crash = false;
        while let Ok(event) = events.try_recv() {
            if let Event::PluginCrash { rolled_back, .. } = event {
                saw_crash |= rolled_back;
            }
        }
        assert!(saw_crash, "a crash event with rolled_back was published");
        assert_eq!(
            store
                .report_crash("me/p", PluginEventKind::Loaded, "x")
                .unwrap_err()
                .code(),
            "invalid_input"
        );
    }

    #[test]
    fn crash_without_snapshot_just_disables() {
        let (store, tmp) = store_with(|c| c.plugins.crash_threshold = 1);
        install(&store, tmp.path(), "me/p", "1.0.0", "", "x()", opts()).unwrap();
        store.enable("me/p").unwrap();
        let outcome = store
            .report_crash("me/p", PluginEventKind::Crash, "boom")
            .unwrap();
        assert!(outcome.disabled && outcome.rolled_back_to.is_none());
        assert!(!store.info("me/p").unwrap().plugin.enabled);
    }

    #[test]
    fn corrupted_snapshot_is_not_restored() {
        let (store, tmp) = store_with(|c| c.plugins.crash_threshold = 1);
        install(&store, tmp.path(), "me/p", "1.0.0", "", "good()", opts()).unwrap();
        install(
            &store,
            tmp.path(),
            "me/p",
            "2.0.0",
            "",
            "bad()",
            opts().replacing(),
        )
        .unwrap();
        store.enable("me/p").unwrap();
        let snap = store
            .db
            .with(|c| plugins::snapshots(c, "me/p"))
            .unwrap()
            .remove(0);
        fs::write(&snap.path, b"tampered").unwrap();
        let outcome = store
            .report_crash("me/p", PluginEventKind::Crash, "boom")
            .unwrap();
        assert!(outcome.disabled && outcome.rolled_back_to.is_none());
        assert_eq!(store.info("me/p").unwrap().plugin.version, "2.0.0");
    }

    #[test]
    fn storage_is_permissioned_namespaced_and_limited() {
        let (store, tmp) = store();
        for id in ["me/a", "me/b"] {
            install(
                &store,
                tmp.path(),
                id,
                "1",
                r#","permissions":["storage"]"#,
                "x()",
                opts(),
            )
            .unwrap();
            store.grant(id, Permission::Storage).unwrap();
            store.enable(id).unwrap();
        }
        let v = serde_json::json!({"n": [1, 2]});
        store.storage_set("me/a", "settings", &v).unwrap();
        assert_eq!(store.storage_get("me/a", "settings").unwrap(), Some(v));
        assert_eq!(
            store.storage_get("me/b", "settings").unwrap(),
            None,
            "plugins do not see each other's data"
        );
        assert!(store.storage_delete("me/a", "settings").unwrap());
        assert!(!store.storage_delete("me/a", "settings").unwrap());

        assert!(store.storage_get("me/a", "bad key").is_err());
        let huge = serde_json::Value::String("x".repeat(MAX_STORAGE_VALUE_BYTES + 1));
        assert_eq!(
            store.storage_set("me/a", "k", &huge).unwrap_err().code(),
            "invalid_input"
        );
        for i in 0..MAX_STORAGE_KEYS {
            store
                .storage_set("me/a", &format!("k{i}"), &serde_json::json!(i))
                .unwrap();
        }
        assert!(store
            .storage_set("me/a", "one-too-many", &serde_json::json!(1))
            .is_err());
        store
            .storage_set("me/a", "k0", &serde_json::json!("existing keys can change"))
            .unwrap();

        store.disable("me/a").unwrap();
        assert_eq!(
            store.storage_get("me/a", "k0").unwrap_err().code(),
            "permission_denied"
        );
    }

    #[test]
    fn remove_deletes_files_snapshots_and_data() {
        let (store, tmp) = store();
        install(
            &store,
            tmp.path(),
            "me/p",
            "1",
            r#","permissions":["storage"]"#,
            "a()",
            opts(),
        )
        .unwrap();
        install(
            &store,
            tmp.path(),
            "me/p",
            "2",
            r#","permissions":["storage"]"#,
            "b()",
            opts().replacing(),
        )
        .unwrap();
        store.grant("me/p", Permission::Storage).unwrap();
        store.enable("me/p").unwrap();
        store
            .storage_set("me/p", "k", &serde_json::json!(1))
            .unwrap();
        // another plugin whose id differs only by a LIKE wildcard character must survive
        install(
            &store,
            tmp.path(),
            "me/pxq",
            "1",
            r#","permissions":["storage"]"#,
            "a()",
            opts(),
        )
        .unwrap();
        store.grant("me/pxq", Permission::Storage).unwrap();
        store.enable("me/pxq").unwrap();
        store
            .storage_set("me/pxq", "k", &serde_json::json!(2))
            .unwrap();

        let snap = store
            .db
            .with(|c| plugins::snapshots(c, "me/p"))
            .unwrap()
            .remove(0);
        store.remove("me/p").unwrap();
        assert!(!store.plugin_dir("me/p").unwrap().exists() && !Path::new(&snap.path).exists());
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(
            store.storage_get("me/pxq", "k").unwrap(),
            Some(serde_json::json!(2))
        );
        assert_eq!(store.remove("me/p").unwrap_err().code(), "not_found");
    }

    #[test]
    fn report_state_is_limited_to_enabled_plugins() {
        let (store, tmp) = store();
        install(&store, tmp.path(), "me/p", "1", "", "x()", opts()).unwrap();
        assert_eq!(
            store
                .report_state("me/p", PluginState::Active)
                .unwrap_err()
                .code(),
            "permission_denied"
        );
        store.enable("me/p").unwrap();
        store.report_state("me/p", PluginState::Active).unwrap();
        assert_eq!(
            store.info("me/p").unwrap().plugin.state,
            PluginState::Active
        );
        assert_eq!(
            store
                .report_state("me/p", PluginState::Removed)
                .unwrap_err()
                .code(),
            "invalid_input"
        );
    }
}
