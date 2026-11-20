//! The marketplace service: refresh, search, install, update and rate.

use crate::fetch::{check_index_url, Fetcher};
use crate::index::{Index, IndexEntry, MAX_PACKAGE_BYTES};
use crate::signed::{self, SignedIndex, TrustedKey, MAX_INDEX_BYTES};
use ghostify_core::config::AutoUpdate;
use ghostify_core::{Config, Error, Event, EventBus, Result};
use ghostify_db::repo::market::{self, Listing, ListingKind, Package, Rating, SearchQuery};
use ghostify_db::repo::misc::{self, ScanVerdict};
use ghostify_db::repo::plugins::PluginSource;
use ghostify_db::repo::settings;
use ghostify_db::repo::themes::ThemeSource;
use ghostify_db::Db;
use ghostify_plugins::store::{Inspection, InstallOptions};
use ghostify_plugins::PluginStore;
use ghostify_themes::ThemeStore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

const KEY_SEQUENCE: &str = "market.sequence";
const KEY_LAST_REFRESH: &str = "market.last_refresh";

pub struct MarketplaceDeps {
    pub db: Db,
    pub plugins: PluginStore,
    pub themes: ThemeStore,
    pub fetcher: Arc<dyn Fetcher>,
    pub config: Arc<Config>,
    /// Keys compiled into the application. Keys of the `trusted_publishers`
    /// table are added to them.
    pub keys: Vec<TrustedKey>,
    pub events: Option<EventBus>,
    /// Directory for downloads in progress.
    pub staging: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum RefreshOutcome {
    /// The last refresh is younger than the configured interval.
    Skipped,
    Applied {
        sequence: u64,
        listings: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateInfo {
    pub listing_id: String,
    pub kind: ListingKind,
    pub current_version: String,
    pub new_version: String,
    /// Permissions the new version asks for beyond what is installed.
    pub new_permissions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutoUpdateResult {
    pub listing_id: String,
    pub new_version: String,
    pub applied: bool,
    /// Why nothing was applied.
    pub reason: Option<String>,
}

pub struct Marketplace {
    deps: MarketplaceDeps,
}

/// Dotted numeric comparison; a pre-release (`1.0.0-beta`) is older than its release.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    fn split(v: &str) -> (Vec<u64>, Option<&str>) {
        let v = v.trim().trim_start_matches('v');
        let v = v.split('+').next().unwrap_or(v);
        let (core, pre) = match v.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (v, None),
        };
        (
            core.split('.').map(|p| p.parse().unwrap_or(0)).collect(),
            pre,
        )
    }
    let ((na, pa), (nb, pb)) = (split(a), split(b));
    for i in 0..na.len().max(nb.len()) {
        let ord = na.get(i).unwrap_or(&0).cmp(nb.get(i).unwrap_or(&0));
        if ord.is_ne() {
            return ord;
        }
    }
    match (pa, pb) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(x), Some(y)) => x.cmp(y),
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

struct Downloaded {
    file: tempfile::NamedTempFile,
    inspection: Option<Inspection>,
}

impl Marketplace {
    pub fn new(deps: MarketplaceDeps) -> Result<Marketplace> {
        std::fs::create_dir_all(&deps.staging)?;
        Ok(Marketplace { deps })
    }

    fn trusted_keys(&self) -> Result<Vec<TrustedKey>> {
        let mut keys = self.deps.keys.clone();
        for p in self.deps.db.with(misc::list_publishers)? {
            if !keys.iter().any(|k| k.key_id == p.key_id) {
                keys.push(TrustedKey {
                    key_id: p.key_id,
                    public_key: p.public_key,
                });
            }
        }
        Ok(keys)
    }

    pub fn sequence(&self) -> Result<u64> {
        Ok(self
            .deps
            .db
            .with(|c| settings::get::<u64>(c, KEY_SEQUENCE))?
            .unwrap_or(0))
    }

    /// Verifies and stores a signed index given as file text.
    pub fn apply_index_text(&self, text: &str) -> Result<RefreshOutcome> {
        let signed: SignedIndex = serde_json::from_str(text)
            .map_err(|e| Error::Invalid(format!("the index file is malformed: {e}")))?;
        let index = signed::verify(&signed, &self.trusted_keys()?, self.sequence()?)?;
        self.store(&index)
    }

    fn store(&self, index: &Index) -> Result<RefreshOutcome> {
        let listings: Vec<Listing> = index.listings.iter().map(IndexEntry::listing).collect();
        let packages: Vec<Package> = index.listings.iter().map(IndexEntry::package).collect();
        let version = i64::try_from(index.sequence)
            .map_err(|_| Error::Invalid("the index sequence is too large".into()))?;
        self.deps.db.with(|c| {
            market::replace_index(c, &listings, version)?;
            market::replace_packages(c, &packages, version)?;
            settings::set(c, KEY_SEQUENCE, &index.sequence)?;
            settings::set(c, KEY_LAST_REFRESH, &now_secs())
        })?;
        Ok(RefreshOutcome::Applied {
            sequence: index.sequence,
            listings: listings.len(),
        })
    }

    /// Loads the index shipped with the application when none has been applied yet.
    pub fn load_bundled(&self, text: &str) -> Result<bool> {
        if self.sequence()? > 0 {
            return Ok(false);
        }
        self.apply_index_text(text)?;
        Ok(true)
    }

    /// Fetches and applies the index. Without `force`, a recent refresh is not repeated.
    pub fn refresh(&self, force: bool) -> Result<RefreshOutcome> {
        let market = &self.deps.config.marketplace;
        if !force {
            let last = self
                .deps
                .db
                .with(|c| settings::get::<i64>(c, KEY_LAST_REFRESH))?
                .unwrap_or(0);
            if now_secs().saturating_sub(last) < market.refresh_interval_secs as i64 {
                return Ok(RefreshOutcome::Skipped);
            }
        }
        check_index_url(&market.index_url)?;
        let bytes = self.deps.fetcher.get(&market.index_url, MAX_INDEX_BYTES)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::Invalid("the index is not valid UTF-8".into()))?;
        self.apply_index_text(&text)
    }

    // ---- browsing --------------------------------------------------------

    pub fn search(&self, query: &SearchQuery) -> Result<Vec<Listing>> {
        self.deps.db.with(|c| market::search(c, query))
    }

    pub fn categories(&self) -> Result<Vec<market::CategoryCount>> {
        self.deps.db.with(|c| market::categories(c, None))
    }

    pub fn get(&self, id: &str) -> Result<Listing> {
        self.deps
            .db
            .with(|c| market::get(c, id))?
            .ok_or_else(|| Error::NotFound(format!("marketplace listing `{id}`")))
    }

    /// Download details of a listing's latest version, including the permissions
    /// it asks for, so they can be shown before installing.
    pub fn package_info(&self, id: &str) -> Result<Package> {
        Ok(self.package(id)?.1)
    }

    pub fn rate(&self, listing_id: &str, score: u8) -> Result<Rating> {
        self.get(listing_id)?;
        self.deps
            .db
            .with(|c| market::set_rating(c, listing_id, score, None))
    }

    pub fn my_rating(&self, listing_id: &str) -> Result<Option<Rating>> {
        self.deps.db.with(|c| market::get_rating(c, listing_id))
    }

    // ---- install and update ---------------------------------------------

    fn package(&self, id: &str) -> Result<(Listing, Package)> {
        let listing = self.get(id)?;
        let package = self
            .deps
            .db
            .with(|c| market::get_package(c, id))?
            .ok_or_else(|| Error::NotFound(format!("no package for `{id}`")))?;
        Ok((listing, package))
    }

    /// Downloads, verifies against the signed index and inspects the archive.
    fn download(&self, listing: &Listing, package: &Package) -> Result<Downloaded> {
        let bytes = self
            .deps
            .fetcher
            .get(&package.download_url, package.size.min(MAX_PACKAGE_BYTES))?;
        if bytes.len() as u64 != package.size {
            return Err(Error::PermissionDenied(format!(
                "the downloaded size of `{}` does not match the index",
                listing.id
            )));
        }
        let digest = Sha256::digest(&bytes);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        if hex != package.sha256 {
            return Err(Error::PermissionDenied(format!(
                "the checksum of `{}` does not match the index",
                listing.id
            )));
        }
        let mut file = tempfile::Builder::new()
            .prefix("pkg-")
            .suffix(".zip")
            .tempfile_in(&self.deps.staging)?;
        file.write_all(&bytes)?;
        file.flush()?;

        let inspection = match listing.kind {
            ListingKind::Plugin => {
                let inspection = self.deps.plugins.inspect_zip(file.path())?;
                let manifest = &inspection.manifest;
                if manifest.id != listing.id || manifest.version != package.version {
                    return Err(Error::PermissionDenied(format!(
                        "the package contains `{}` {} but the index promised `{}` {}",
                        manifest.id, manifest.version, listing.id, package.version
                    )));
                }
                let allowed: BTreeSet<&str> =
                    package.permissions.iter().map(String::as_str).collect();
                if let Some(extra) = manifest
                    .permissions
                    .iter()
                    .find(|p| !allowed.contains(p.as_str()))
                {
                    return Err(Error::PermissionDenied(format!(
                        "the package asks for `{}` which the index does not announce",
                        extra.as_str()
                    )));
                }
                Some(inspection)
            }
            ListingKind::Theme => {
                let manifest = self.deps.themes.inspect_zip(file.path())?;
                if manifest.id != listing.id || manifest.version != package.version {
                    return Err(Error::PermissionDenied(format!(
                        "the package contains `{}` {} but the index promised `{}` {}",
                        manifest.id, manifest.version, listing.id, package.version
                    )));
                }
                None
            }
        };
        Ok(Downloaded { file, inspection })
    }

    fn install_downloaded(
        &self,
        listing: &Listing,
        down: &Downloaded,
        replace: bool,
        allow_blocked: bool,
    ) -> Result<()> {
        match listing.kind {
            ListingKind::Plugin => {
                let mut options = InstallOptions::new(PluginSource::Marketplace);
                options.replace = replace;
                options.allow_blocked = allow_blocked;
                self.deps.plugins.install_zip(down.file.path(), options)?;
            }
            ListingKind::Theme => {
                self.deps.themes.install_zip(
                    down.file.path(),
                    ThemeSource::Marketplace,
                    replace,
                )?;
            }
        }
        Ok(())
    }

    /// Installs a listing. `allow_blocked` is only for an explicit decision of the user
    /// after the scan blocked the package.
    pub fn install(&self, id: &str, allow_blocked: bool) -> Result<()> {
        let (listing, package) = self.package(id)?;
        let down = self.download(&listing, &package)?;
        self.install_downloaded(&listing, &down, false, allow_blocked)
    }

    /// Installed marketplace items with a newer version in the index.
    pub fn updates(&self) -> Result<Vec<UpdateInfo>> {
        let mut found = Vec::new();
        for info in self.deps.plugins.list()? {
            if info.plugin.source != PluginSource::Marketplace {
                continue;
            }
            let Some(listing) = self.deps.db.with(|c| market::get(c, &info.plugin.id))? else {
                continue;
            };
            if listing.kind != ListingKind::Plugin
                || compare_versions(&listing.latest_version, &info.plugin.version).is_le()
            {
                continue;
            }
            let package = self.deps.db.with(|c| market::get_package(c, &listing.id))?;
            let known: BTreeSet<&String> = info.requested.iter().collect();
            let new_permissions = package
                .map(|p| {
                    p.permissions
                        .into_iter()
                        .filter(|perm| !known.contains(perm))
                        .collect()
                })
                .unwrap_or_default();
            found.push(UpdateInfo {
                listing_id: listing.id,
                kind: ListingKind::Plugin,
                current_version: info.plugin.version,
                new_version: listing.latest_version,
                new_permissions,
            });
        }
        for info in self.deps.themes.list()? {
            if info.theme.source != ThemeSource::Marketplace {
                continue;
            }
            let Some(listing) = self.deps.db.with(|c| market::get(c, &info.theme.id))? else {
                continue;
            };
            if listing.kind == ListingKind::Theme
                && compare_versions(&listing.latest_version, &info.theme.version).is_gt()
            {
                found.push(UpdateInfo {
                    listing_id: listing.id,
                    kind: ListingKind::Theme,
                    current_version: info.theme.version,
                    new_version: listing.latest_version,
                    new_permissions: Vec::new(),
                });
            }
        }
        Ok(found)
    }

    fn apply_update(
        &self,
        update: &UpdateInfo,
        allow_new_permissions: bool,
        require_clean: bool,
    ) -> Result<()> {
        if !update.new_permissions.is_empty() && !allow_new_permissions {
            return Err(Error::PermissionDenied(format!(
                "`{}` {} asks for new permissions ({}); confirm them first",
                update.listing_id,
                update.new_version,
                update.new_permissions.join(", ")
            )));
        }
        let (listing, package) = self.package(&update.listing_id)?;
        let down = self.download(&listing, &package)?;
        if require_clean
            && down
                .inspection
                .as_ref()
                .is_some_and(|i| i.report.verdict != ScanVerdict::Clean)
        {
            return Err(Error::PermissionDenied(format!(
                "the scan of `{}` found something that needs a look",
                listing.id
            )));
        }
        self.install_downloaded(&listing, &down, true, false)
    }

    /// Updates one installed item. New permissions need `allow_new_permissions`.
    pub fn update(&self, id: &str, allow_new_permissions: bool) -> Result<UpdateInfo> {
        let update = self
            .updates()?
            .into_iter()
            .find(|u| u.listing_id == id)
            .ok_or_else(|| Error::NotFound(format!("no update for `{id}`")))?;
        self.apply_update(&update, allow_new_permissions, false)?;
        Ok(update)
    }

    /// Announces every update and applies those the policy allows.
    pub fn auto_update(&self) -> Result<Vec<AutoUpdateResult>> {
        let policy = self.deps.config.plugins.auto_update;
        let mut results = Vec::new();
        for update in self.updates()? {
            if let Some(bus) = &self.deps.events {
                bus.publish(Event::MarketUpdateAvailable {
                    listing_id: update.listing_id.clone(),
                    current_version: update.current_version.clone(),
                    new_version: update.new_version.clone(),
                });
            }
            let outcome = match policy {
                AutoUpdate::Off => Err("automatic updates are off".to_owned()),
                _ if !update.new_permissions.is_empty() => {
                    Err("the new version asks for new permissions".to_owned())
                }
                policy => self
                    .apply_update(&update, false, policy == AutoUpdate::SafeOnly)
                    .map_err(|e| e.to_string()),
            };
            results.push(AutoUpdateResult {
                listing_id: update.listing_id,
                new_version: update.new_version,
                applied: outcome.is_ok(),
                reason: outcome.err(),
            });
        }
        Ok(results)
    }
}
