//! Wires the Ghostify crates into one application.
//!
//! [`Core`] holds everything the command line needs (database, stores, patch
//! manager, marketplace). [`App`] adds what only a running instance needs: the
//! bridge to the loader inside Spotify, live theme reload, automatic repatching
//! and the background marketplace refresh.

pub mod api;
pub mod builtin;

use ghostify_bridge::{secret, Bridge, BridgeConfig, Router};
use ghostify_core::{AppPaths, Config, Error, EventBus, Result};
use ghostify_db::Db;
use ghostify_extras::discord;
use ghostify_extras::lyrics::ONLINE_HOST;
use ghostify_extras::{HotkeyService, LyricsService, PlayerHub, PlayerLink};
use ghostify_market::{Fetcher, Marketplace, MarketplaceDeps, TrustedKey, UreqFetcher};
use ghostify_patch::{AutoRepatch, LoaderConfig, PatchManager, StaticSource};
use ghostify_plugins::PluginStore;
use ghostify_themes::{ThemeService, ThemeStore};
use serde_json::Value;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::task::JoinHandle;

/// Loader script and stylesheet built from `packages/runtime` (`pnpm build`,
/// then `scripts/sync-runtime.sh`).
pub const LOADER_JS: &[u8] = include_bytes!("../assets/loader.js");
pub const LOADER_CSS: &[u8] = include_bytes!("../assets/loader.css");

/// Ports the loader tries by default.
pub const DEFAULT_BRIDGE_PORTS: std::ops::RangeInclusive<u16> = 49421..=49430;

/// Signing key of the project's marketplace registry, set at build time as
/// `GHOSTIFY_MARKET_KEY=<key id>:<base64 public key>` (see `ghostify-registry keygen`).
pub fn builtin_market_keys() -> Vec<TrustedKey> {
    option_env!("GHOSTIFY_MARKET_KEY")
        .and_then(|value| value.split_once(':'))
        .map(|(key_id, public_key)| {
            vec![TrustedKey {
                key_id: key_id.to_owned(),
                public_key: public_key.to_owned(),
            }]
        })
        .unwrap_or_default()
}

/// Ports the bridge listens on and the loader connects to. `bridge.port = 0`
/// means the default range; any other value pins one port.
pub fn bridge_ports(config: &Config) -> Vec<u16> {
    match config.bridge.port {
        0 => DEFAULT_BRIDGE_PORTS.collect(),
        port => vec![port],
    }
}

pub struct CoreOptions {
    pub paths: AppPaths,
    pub config: Config,
    pub market_keys: Vec<TrustedKey>,
    pub fetcher: Arc<dyn Fetcher>,
    /// Used for the optional lyrics lookup; `None` disables it.
    pub lyrics_fetcher: Option<Arc<dyn Fetcher>>,
    /// Replaces the detection of Spotify installations (tests).
    pub detector: Option<Arc<dyn Fn() -> Vec<ghostify_spotify::DetectedInstall> + Send + Sync>>,
}

impl CoreOptions {
    /// Paths and configuration of the current user.
    pub fn discover() -> Result<CoreOptions> {
        let paths = AppPaths::discover()?;
        let config = Config::load(&paths.config_file())?;
        Ok(CoreOptions::new(paths, config))
    }

    pub fn new(paths: AppPaths, config: Config) -> CoreOptions {
        CoreOptions {
            paths,
            config,
            market_keys: builtin_market_keys(),
            fetcher: Arc::new(UreqFetcher::new()),
            lyrics_fetcher: Some(Arc::new(UreqFetcher::with_extra_hosts(&[ONLINE_HOST]))),
            detector: None,
        }
    }
}

pub struct Core {
    pub paths: AppPaths,
    pub config: Arc<Config>,
    pub db: Db,
    pub events: EventBus,
    pub patch: PatchManager,
    pub themes: ThemeStore,
    pub plugins: PluginStore,
    pub market: Arc<Marketplace>,
    pub player: PlayerHub,
    pub lyrics: Arc<LyricsService>,
    pub hotkeys: Arc<HotkeyService>,
    pub bridge_secret: String,
}

impl Core {
    pub fn open(options: CoreOptions) -> Result<Core> {
        let CoreOptions {
            paths,
            config,
            market_keys,
            fetcher,
            lyrics_fetcher,
            detector,
        } = options;
        config.validate()?;
        paths.ensure()?;
        let config = Arc::new(config);
        let db = Db::open(&paths.database_file())?;
        let events = EventBus::new(256);
        let bridge_secret = secret::load_or_create(&db)?;

        let loader = LoaderConfig::new(
            bridge_ports(&config),
            bridge_secret.clone(),
            env!("CARGO_PKG_VERSION"),
        )?;
        let source = StaticSource::builtin(LOADER_JS, LOADER_CSS, &loader)?;
        let mut patch = PatchManager::new(
            db.clone(),
            &paths,
            config.clone(),
            events.clone(),
            Arc::new(source),
        );
        if let Some(detector) = detector {
            patch = patch.with_detector(move || detector());
        }

        let themes = ThemeStore::new(db.clone(), &paths)?;
        if let Err(error) = builtin::install_builtin_themes(&themes, &paths.staging_dir()) {
            tracing::warn!(%error, "could not install the bundled theme");
        }
        let plugins =
            PluginStore::new(db.clone(), &paths, config.clone())?.with_events(events.clone());
        let market = Arc::new(Marketplace::new(MarketplaceDeps {
            db: db.clone(),
            plugins: plugins.clone(),
            themes: themes.clone(),
            fetcher,
            config: config.clone(),
            keys: market_keys,
            events: Some(events.clone()),
            staging: paths.staging_dir().join("market"),
        })?);
        let lyrics_dir = paths.data_dir().join("lyrics");
        std::fs::create_dir_all(&lyrics_dir)?;
        let lyrics = Arc::new(LyricsService::new(
            db.clone(),
            lyrics_dir,
            lyrics_fetcher,
            config.extras.lyrics_online,
        ));
        let hotkeys = Arc::new(HotkeyService::new(db.clone()));
        Ok(Core {
            paths,
            config,
            db,
            events,
            patch,
            themes,
            plugins,
            market,
            player: PlayerHub::new(),
            lyrics,
            hotkeys,
            bridge_secret,
        })
    }
}

/// Interval at which the background task checks whether the index is due.
const MARKET_TICK: Duration = Duration::from_secs(15 * 60);

pub struct App {
    pub core: Core,
    pub themes: Arc<ThemeService>,
    bridge: Arc<RwLock<Option<Bridge>>>,
    bridge_port: u16,
    tasks: Vec<JoinHandle<()>>,
    auto_repatch: Option<AutoRepatch>,
}

impl App {
    pub async fn start(core: Core) -> Result<App> {
        // The bridge needs the router, the router needs services that push through
        // the bridge: the notifier looks the bridge up once it exists.
        let bridge: Arc<RwLock<Option<Bridge>>> = Arc::new(RwLock::new(None));
        let notifier: ghostify_themes::service::Notifier = {
            let bridge = bridge.clone();
            Arc::new(move |method: &str, params: &Value| {
                if let Some(bridge) = bridge.read().unwrap_or_else(|p| p.into_inner()).as_ref() {
                    bridge.notify(method, params);
                }
            })
        };

        let themes = ThemeService::new(
            core.themes.clone(),
            notifier.clone(),
            Duration::from_millis(150),
        );
        let mut router = Router::with_builtin(core.db.clone());
        themes.register(&mut router);
        ghostify_plugins::rpc::register(&core.plugins, &mut router);
        register_market(&core.market, &mut router);
        core.player.register(&mut router);

        let mut bridge_config = BridgeConfig::new(core.bridge_secret.clone());
        bridge_config.ports = bridge_ports(&core.config);
        bridge_config.max_message_bytes = core.config.bridge.max_message_bytes;
        let started = Bridge::start(bridge_config, router).await?;
        let bridge_port = started.port();
        *bridge.write().unwrap_or_else(|p| p.into_inner()) = Some(started);
        core.player.set_link(PlayerLink {
            notify: notifier.clone(),
            connected: {
                let bridge = bridge.clone();
                Arc::new(move || {
                    bridge
                        .read()
                        .unwrap_or_else(|p| p.into_inner())
                        .as_ref()
                        .map_or(0, Bridge::connected)
                })
            },
        });

        let mut tasks = vec![ghostify_plugins::rpc::forward_changes(
            &core.events,
            notifier,
        )];
        themes.start().await?;

        let auto_repatch = if core.config.patch.auto_repatch {
            Some(
                AutoRepatch::start(
                    core.patch.clone(),
                    Duration::from_millis(core.config.patch.repatch_debounce_ms),
                )
                .await?,
            )
        } else {
            None
        };
        tasks.push(spawn_market_loop(core.market.clone()));
        tasks.extend(start_discord(&core));

        Ok(App {
            core,
            themes,
            bridge,
            bridge_port,
            tasks,
            auto_repatch,
        })
    }

    pub fn bridge_port(&self) -> u16 {
        self.bridge_port
    }

    pub fn connected_loaders(&self) -> usize {
        self.bridge
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map_or(0, Bridge::connected)
    }

    pub async fn shutdown(mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
        drop(self.auto_repatch.take());
        let bridge = self
            .bridge
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(bridge) = bridge {
            bridge.shutdown().await;
        }
    }
}

/// Read-only marketplace methods for a marketplace page inside Spotify.
/// Installing and updating stay in the Ghostify app: they change what runs.
fn register_market(market: &Arc<Marketplace>, router: &mut Router) {
    use ghostify_bridge::router::parse;
    use ghostify_db::repo::market::SearchQuery;

    let search = market.clone();
    router.register("market.search", move |_, params| {
        let market = search.clone();
        async move {
            let query: SearchQuery = parse(params)?;
            let found = blocking(move || market.search(&query)).await?;
            Ok(serde_json::to_value(found).map_err(Error::from)?)
        }
    });
    let get = market.clone();
    router.register("market.get", move |_, params| {
        let market = get.clone();
        async move {
            #[derive(serde::Deserialize)]
            struct Id {
                id: String,
            }
            let Id { id } = parse(params)?;
            let listing = blocking(move || market.get(&id)).await?;
            Ok(serde_json::to_value(listing).map_err(Error::from)?)
        }
    });
    let categories = market.clone();
    router.register("market.categories", move |_, _| {
        let market = categories.clone();
        async move {
            let found = blocking(move || market.categories()).await?;
            Ok(serde_json::to_value(found).map_err(Error::from)?)
        }
    });
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Error::Internal(format!("task failed: {e}")))?
}

/// Refreshes the index when it is due and applies the update policy.
fn spawn_market_loop(market: Arc<Marketplace>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let m = market.clone();
            let result = blocking(move || {
                let refreshed = m.refresh(false)?;
                let updates = m.auto_update()?;
                Ok((refreshed, updates))
            })
            .await;
            match result {
                Ok((refreshed, updates)) => {
                    tracing::debug!(?refreshed, updates = updates.len(), "marketplace checked")
                }
                // Offline is normal: the cached index keeps working.
                Err(err) => tracing::warn!(%err, "marketplace refresh failed"),
            }
            tokio::time::sleep(MARKET_TICK).await;
        }
    })
}

/// Mirrors the player state to Discord when the user turned that on. The
/// blocking IPC runs on its own thread; dropping the sender (when this task is
/// aborted) makes it clear the presence and exit.
fn start_discord(core: &Core) -> Option<JoinHandle<()>> {
    let extras = &core.config.extras;
    if !extras.discord_enabled {
        return None;
    }
    let client_id = extras.discord_client_id.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("ghostify-discord".into())
        .spawn(move || {
            discord::run_presence(
                rx,
                client_id,
                discord::connect_ipc,
                || {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                },
                Duration::from_secs(15),
            )
        })
        .ok()?;
    let hub = core.player.clone();
    Some(tokio::spawn(async move {
        use tokio::sync::broadcast::error::RecvError;
        let mut updates = hub.subscribe();
        let _ = tx.send(hub.snapshot());
        loop {
            let snapshot = match updates.recv().await {
                Ok(snapshot) => snapshot,
                Err(RecvError::Lagged(_)) => hub.snapshot(),
                Err(RecvError::Closed) => break,
            };
            if tx.send(snapshot).is_err() {
                break;
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_ports_follow_the_configuration() {
        let mut config = Config::default();
        assert_eq!(
            bridge_ports(&config),
            DEFAULT_BRIDGE_PORTS.collect::<Vec<_>>()
        );
        config.bridge.port = 50000;
        assert_eq!(bridge_ports(&config), vec![50000]);
    }

    #[test]
    fn the_embedded_loader_is_present() {
        assert!(LOADER_JS.len() > 1000 && LOADER_CSS.len() > 10);
    }
}
