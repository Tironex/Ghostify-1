use ed25519_dalek::SigningKey;
use ghostify_core::config::AutoUpdate;
use ghostify_core::{AppPaths, Config, Error, Event, EventBus, Result};
use ghostify_db::repo::market::{ListingKind, SearchQuery};
use ghostify_db::repo::misc;
use ghostify_db::repo::plugins::PluginSource;
use ghostify_db::Db;
use ghostify_market::index::{Index, IndexEntry};
use ghostify_market::service::{compare_versions, RefreshOutcome};
use ghostify_market::signed::{self, TrustedKey};
use ghostify_market::{Fetcher, Marketplace, MarketplaceDeps};
use ghostify_plugins::PluginStore;
use ghostify_themes::ThemeStore;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

const INDEX_URL: &str = "https://ghostify.github.io/registry/index.json";

#[derive(Default)]
struct FakeFetcher {
    files: Mutex<HashMap<String, Vec<u8>>>,
    requests: Mutex<Vec<String>>,
}

impl FakeFetcher {
    fn put(&self, url: &str, body: Vec<u8>) {
        self.files.lock().unwrap().insert(url.to_owned(), body);
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Fetcher for FakeFetcher {
    fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>> {
        self.requests.lock().unwrap().push(url.to_owned());
        let body = self
            .files
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("no such url {url}")))?;
        if body.len() as u64 > max_bytes {
            return Err(Error::Invalid("too large".into()));
        }
        Ok(body)
    }
}

fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, content) in files {
        writer.start_file(*name, options).unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn plugin_zip(id: &str, version: &str, permissions: &[&str]) -> Vec<u8> {
    let hosts = if permissions.contains(&"network") {
        r#","hosts":["example.com"]"#
    } else {
        ""
    };
    let perms: Vec<String> = permissions.iter().map(|p| format!("\"{p}\"")).collect();
    let manifest = format!(
        r#"{{"id":"{id}","name":"Plugin {id}","version":"{version}","permissions":[{}]{hosts}}}"#,
        perms.join(",")
    );
    zip_of(&[
        ("plugin.json", &manifest),
        ("index.js", "ghostify.ui.log('hello');"),
    ])
}

fn theme_zip(id: &str, version: &str) -> Vec<u8> {
    let manifest = format!(r#"{{"id":"{id}","name":"Theme {id}","version":"{version}"}}"#);
    zip_of(&[
        ("theme.json", &manifest),
        ("user.css", "body { color: red; }"),
    ])
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn url_for(id: &str, version: &str) -> String {
    format!(
        "https://github.com/owner/repo/releases/download/v{version}/{}.zip",
        id.replace('/', "-")
    )
}

fn entry(
    id: &str,
    kind: ListingKind,
    version: &str,
    zip: &[u8],
    permissions: &[&str],
) -> IndexEntry {
    IndexEntry {
        id: id.into(),
        kind,
        name: format!("Name of {id}"),
        author: "author".into(),
        repo: "owner/repo".into(),
        category: "tools".into(),
        description: "lyrics and more".into(),
        tags: vec!["lyrics".into()],
        latest_version: version.into(),
        stars: 5,
        rating_avg: None,
        rating_count: 0,
        download_url: url_for(id, version),
        sha256: sha(zip),
        size: zip.len() as u64,
        permissions: permissions.iter().map(|p| (*p).to_owned()).collect(),
    }
}

struct World {
    market: Marketplace,
    fetcher: Arc<FakeFetcher>,
    plugins: PluginStore,
    themes: ThemeStore,
    db: Db,
    key: SigningKey,
    bus: EventBus,
    _tmp: tempfile::TempDir,
}

fn world_with(auto_update: AutoUpdate) -> World {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::from_root(tmp.path());
    let mut config = Config::default();
    config.plugins.auto_update = auto_update;
    let config = Arc::new(config);
    let db = Db::open_in_memory().unwrap();
    let bus = EventBus::new(32);
    let plugins = PluginStore::new(db.clone(), &paths, config.clone())
        .unwrap()
        .with_events(bus.clone());
    let themes = ThemeStore::new(db.clone(), &paths).unwrap();
    let fetcher = Arc::new(FakeFetcher::default());
    let key = signed::generate_key().unwrap();
    let market = Marketplace::new(MarketplaceDeps {
        db: db.clone(),
        plugins: plugins.clone(),
        themes: themes.clone(),
        fetcher: fetcher.clone(),
        config,
        keys: vec![TrustedKey {
            key_id: "main".into(),
            public_key: signed::encode_public_key(&key),
        }],
        events: Some(bus.clone()),
        staging: paths.staging_dir().join("market"),
    })
    .unwrap();
    World {
        market,
        fetcher,
        plugins,
        themes,
        db,
        key,
        bus,
        _tmp: tmp,
    }
}

fn world() -> World {
    world_with(AutoUpdate::SafeOnly)
}

impl World {
    fn signed_text(&self, sequence: u64, listings: Vec<IndexEntry>) -> String {
        let index = Index {
            sequence,
            generated_at: 1_700_000_000,
            listings,
        };
        serde_json::to_string(&signed::sign(&index, "main", &self.key).unwrap()).unwrap()
    }

    /// Publishes an index with the given entries and refreshes from it.
    fn publish(&self, sequence: u64, listings: Vec<IndexEntry>) -> RefreshOutcome {
        self.fetcher
            .put(INDEX_URL, self.signed_text(sequence, listings).into_bytes());
        self.market.refresh(true).unwrap()
    }

    fn host_plugin(&self, id: &str, version: &str, permissions: &[&str]) -> IndexEntry {
        let zip = plugin_zip(id, version, permissions);
        let e = entry(id, ListingKind::Plugin, version, &zip, permissions);
        self.fetcher.put(&e.download_url, zip);
        e
    }

    fn host_theme(&self, id: &str, version: &str) -> IndexEntry {
        let zip = theme_zip(id, version);
        let e = entry(id, ListingKind::Theme, version, &zip, &[]);
        self.fetcher.put(&e.download_url, zip);
        e
    }
}

fn query(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        ..SearchQuery::default()
    }
}

#[test]
fn refresh_applies_the_index_and_search_finds_listings() {
    let w = world();
    let plugin = w.host_plugin("me/lyrics", "1.0.0", &[]);
    let theme = w.host_theme("me/dark", "1.0.0");
    assert_eq!(
        w.publish(3, vec![plugin, theme]),
        RefreshOutcome::Applied {
            sequence: 3,
            listings: 2
        }
    );
    assert_eq!(w.market.sequence().unwrap(), 3);
    assert_eq!(w.market.search(&query("lyr")).unwrap().len(), 2);
    let only_themes = SearchQuery {
        kind: Some(ListingKind::Theme),
        ..query("")
    };
    assert_eq!(w.market.search(&only_themes).unwrap()[0].id, "me/dark");
    assert_eq!(w.market.get("me/lyrics").unwrap().latest_version, "1.0.0");
    assert_eq!(w.market.get("none").unwrap_err().code(), "not_found");
    assert_eq!(w.market.categories().unwrap()[0].category, "tools");
}

#[test]
fn refresh_respects_the_interval_unless_forced() {
    let w = world();
    w.publish(1, vec![w.host_plugin("me/a", "1.0.0", &[])]);
    let before = w.fetcher.count();
    assert_eq!(w.market.refresh(false).unwrap(), RefreshOutcome::Skipped);
    assert_eq!(w.fetcher.count(), before, "no request within the interval");
    assert!(matches!(
        w.market.refresh(true).unwrap(),
        RefreshOutcome::Applied { .. }
    ));
    assert_eq!(w.fetcher.count(), before + 1);
}

#[test]
fn a_bad_refresh_keeps_the_previous_index() {
    let w = world();
    w.publish(5, vec![w.host_plugin("me/a", "1.0.0", &[])]);

    // Older sequence (replay), foreign key, tampered payload, garbage, missing file.
    let replay = w.signed_text(4, vec![]);
    let stranger = signed::generate_key().unwrap();
    let foreign = serde_json::to_string(
        &signed::sign(
            &Index {
                sequence: 9,
                generated_at: 0,
                listings: vec![],
            },
            "main",
            &stranger,
        )
        .unwrap(),
    )
    .unwrap();
    let tampered = w
        .signed_text(6, vec![])
        .replace("\"sequence\\\":6", "\"sequence\\\":60");
    for body in [replay, foreign, tampered, "not json".to_owned()] {
        w.fetcher.put(INDEX_URL, body.into_bytes());
        assert!(w.market.refresh(true).is_err());
        assert_eq!(w.market.sequence().unwrap(), 5);
        assert_eq!(w.market.search(&query("")).unwrap().len(), 1);
    }
    w.fetcher.files.lock().unwrap().remove(INDEX_URL);
    assert!(w.market.refresh(true).is_err());
}

#[test]
fn publishers_from_the_database_are_trusted_too() {
    let w = world();
    let other = signed::generate_key().unwrap();
    let index = Index {
        sequence: 1,
        generated_at: 0,
        listings: vec![],
    };
    let text = serde_json::to_string(&signed::sign(&index, "partner", &other).unwrap()).unwrap();
    assert!(w.market.apply_index_text(&text).is_err());
    let public = signed::encode_public_key(&other);
    w.db.with(|c| misc::add_publisher(c, "partner", "Partner", &public))
        .unwrap();
    assert!(w.market.apply_index_text(&text).is_ok());
}

#[test]
fn the_bundled_index_is_used_only_before_any_refresh() {
    let w = world();
    let bundled = w.signed_text(1, vec![w.host_plugin("me/a", "1.0.0", &[])]);
    assert!(w.market.load_bundled(&bundled).unwrap());
    assert!(!w.market.load_bundled(&bundled).unwrap());
    assert_eq!(w.market.search(&query("")).unwrap().len(), 1);
}

#[test]
fn installing_a_plugin_verifies_and_records_the_source() {
    let w = world();
    w.publish(1, vec![w.host_plugin("me/lyrics", "1.2.0", &["storage"])]);
    w.market.install("me/lyrics", false).unwrap();
    let info = w.plugins.info("me/lyrics").unwrap();
    assert_eq!(info.plugin.version, "1.2.0");
    assert_eq!(info.plugin.source, PluginSource::Marketplace);
    assert_eq!(info.requested, vec!["storage".to_owned()]);
    assert!(info.granted.is_empty(), "installing grants nothing");
    assert_eq!(
        w.market.install("me/lyrics", false).unwrap_err().code(),
        "conflict"
    );
    assert_eq!(
        w.market.install("me/unknown", false).unwrap_err().code(),
        "not_found"
    );
}

#[test]
fn installing_a_theme_works() {
    let w = world();
    w.publish(1, vec![w.host_theme("me/dark", "2.0.0")]);
    w.market.install("me/dark", false).unwrap();
    let info = w.themes.info("me/dark").unwrap();
    assert_eq!(info.theme.version, "2.0.0");
    assert_eq!(
        info.theme.source,
        ghostify_db::repo::themes::ThemeSource::Marketplace
    );
}

#[test]
fn a_package_that_differs_from_the_signed_index_is_refused() {
    let w = world();
    let good = w.host_plugin("me/a", "1.0.0", &[]);

    // Same size, other content: the checksum catches it.
    let mut swapped = plugin_zip("me/a", "1.0.0", &[]);
    let last = swapped.len() - 30;
    swapped[last] ^= 0x01;
    w.fetcher.put(&good.download_url, swapped);
    w.publish(1, vec![good.clone()]);
    assert_eq!(
        w.market.install("me/a", false).unwrap_err().code(),
        "permission_denied"
    );

    // Different size.
    w.fetcher.put(&good.download_url, b"short".to_vec());
    assert_eq!(
        w.market.install("me/a", false).unwrap_err().code(),
        "permission_denied"
    );
    assert!(
        w.plugins.list().unwrap().is_empty(),
        "nothing was installed"
    );
}

#[test]
fn the_manifest_must_match_what_the_index_announces() {
    let w = world();
    // Another id inside the zip.
    let zip = plugin_zip("me/other", "1.0.0", &[]);
    let wrong_id = entry("me/a", ListingKind::Plugin, "1.0.0", &zip, &[]);
    w.fetcher.put(&wrong_id.download_url, zip);
    // Another version.
    let zip = plugin_zip("me/b", "9.9.9", &[]);
    let wrong_version = entry("me/b", ListingKind::Plugin, "1.0.0", &zip, &[]);
    w.fetcher.put(&wrong_version.download_url, zip);
    // A permission the index does not announce.
    let zip = plugin_zip("me/c", "1.0.0", &["network", "storage"]);
    let hidden = entry("me/c", ListingKind::Plugin, "1.0.0", &zip, &["storage"]);
    w.fetcher.put(&hidden.download_url, zip);
    // A theme zip listed under another id.
    let zip = theme_zip("me/t", "1.0.0");
    let wrong_theme = entry("me/x", ListingKind::Theme, "1.0.0", &zip, &[]);
    w.fetcher.put(&wrong_theme.download_url, zip);

    w.publish(1, vec![wrong_id, wrong_version, hidden, wrong_theme]);
    for id in ["me/a", "me/b", "me/c", "me/x"] {
        assert_eq!(
            w.market.install(id, false).unwrap_err().code(),
            "permission_denied",
            "{id}"
        );
    }
    assert!(w.plugins.list().unwrap().is_empty() && w.themes.list().unwrap().is_empty());
}

#[test]
fn updates_are_found_and_applied() {
    let w = world();
    w.publish(
        1,
        vec![
            w.host_plugin("me/a", "1.0.0", &["storage"]),
            w.host_theme("me/t", "1.0.0"),
        ],
    );
    w.market.install("me/a", false).unwrap();
    w.market.install("me/t", false).unwrap();
    assert!(w.market.updates().unwrap().is_empty());

    w.publish(
        2,
        vec![
            w.host_plugin("me/a", "1.1.0", &["storage"]),
            w.host_theme("me/t", "1.0.1"),
        ],
    );
    let updates = w.market.updates().unwrap();
    assert_eq!(updates.len(), 2);
    assert!(updates.iter().all(|u| u.new_permissions.is_empty()));

    let done = w.market.update("me/a", false).unwrap();
    assert_eq!(
        (done.current_version.as_str(), done.new_version.as_str()),
        ("1.0.0", "1.1.0")
    );
    w.market.update("me/t", false).unwrap();
    assert_eq!(w.plugins.info("me/a").unwrap().plugin.version, "1.1.0");
    assert_eq!(w.themes.info("me/t").unwrap().theme.version, "1.0.1");
    assert!(w.market.updates().unwrap().is_empty());
    assert_eq!(
        w.market.update("me/a", false).unwrap_err().code(),
        "not_found"
    );
}

#[test]
fn plugins_not_installed_from_the_marketplace_are_left_alone() {
    let w = world();
    w.publish(1, vec![w.host_plugin("me/a", "1.0.0", &[])]);
    let src = w._tmp.path().join("local");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("plugin.json"),
        r#"{"id":"me/a","name":"Mine","version":"0.1.0"}"#,
    )
    .unwrap();
    std::fs::write(src.join("index.js"), "ghostify.ui.log('x');").unwrap();
    w.plugins
        .install_dir(
            &src,
            ghostify_plugins::store::InstallOptions::new(PluginSource::Local),
        )
        .unwrap();
    assert!(w.market.updates().unwrap().is_empty());
}

#[test]
fn new_permissions_need_confirmation() {
    let w = world();
    w.publish(1, vec![w.host_plugin("me/a", "1.0.0", &["storage"])]);
    w.market.install("me/a", false).unwrap();
    w.publish(
        2,
        vec![w.host_plugin("me/a", "2.0.0", &["storage", "network"])],
    );

    let update = w.market.updates().unwrap().remove(0);
    assert_eq!(update.new_permissions, vec!["network".to_owned()]);
    assert_eq!(
        w.market.update("me/a", false).unwrap_err().code(),
        "permission_denied"
    );
    assert_eq!(w.plugins.info("me/a").unwrap().plugin.version, "1.0.0");

    w.market.update("me/a", true).unwrap();
    let info = w.plugins.info("me/a").unwrap();
    assert_eq!(info.plugin.version, "2.0.0");
    assert!(
        info.granted.is_empty(),
        "an update never grants permissions"
    );
}

fn drain(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> Vec<Event> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

fn announced(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::MarketUpdateAvailable { listing_id, .. } => Some(listing_id.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn auto_update_follows_the_policy() {
    for (policy, applies_safe) in [
        (AutoUpdate::Off, false),
        (AutoUpdate::SafeOnly, true),
        (AutoUpdate::All, true),
    ] {
        let w = world_with(policy);
        let mut rx = w.bus.subscribe();
        w.publish(
            1,
            vec![
                w.host_plugin("me/safe", "1.0.0", &[]),
                w.host_plugin("me/risky", "1.0.0", &[]),
            ],
        );
        w.market.install("me/safe", false).unwrap();
        w.market.install("me/risky", false).unwrap();
        w.publish(
            2,
            vec![
                w.host_plugin("me/safe", "1.1.0", &[]),
                w.host_plugin("me/risky", "1.1.0", &["network"]),
            ],
        );
        drain(&mut rx);

        let results = w.market.auto_update().unwrap();
        assert_eq!(results.len(), 2, "{policy:?}");
        let safe = results.iter().find(|r| r.listing_id == "me/safe").unwrap();
        let risky = results.iter().find(|r| r.listing_id == "me/risky").unwrap();
        assert_eq!(safe.applied, applies_safe, "{policy:?}");
        assert!(
            !risky.applied && risky.reason.is_some(),
            "new permissions are never applied automatically"
        );
        assert_eq!(
            w.plugins.info("me/safe").unwrap().plugin.version,
            if applies_safe { "1.1.0" } else { "1.0.0" }
        );
        assert_eq!(w.plugins.info("me/risky").unwrap().plugin.version, "1.0.0");

        let mut ids = announced(&drain(&mut rx));
        ids.sort();
        assert_eq!(ids, ["me/risky", "me/safe"], "every update is announced");
    }
}

#[test]
fn safe_only_requires_a_clean_scan() {
    let w = world_with(AutoUpdate::SafeOnly);
    w.publish(1, vec![w.host_plugin("me/a", "1.0.0", &[])]);
    w.market.install("me/a", false).unwrap();
    // The new version is signed and matches the index but the scanner finds an eval.
    let manifest = r#"{"id":"me/a","name":"Plugin me/a","version":"1.1.0"}"#;
    let zip = zip_of(&[
        ("plugin.json", manifest),
        ("index.js", "eval(ghostify.ui.name);"),
    ]);
    let e = entry("me/a", ListingKind::Plugin, "1.1.0", &zip, &[]);
    w.fetcher.put(&e.download_url, zip);
    w.publish(2, vec![e]);
    let results = w.market.auto_update().unwrap();
    assert!(!results[0].applied, "{results:?}");
    assert_eq!(w.plugins.info("me/a").unwrap().plugin.version, "1.0.0");
}

#[test]
fn ratings_are_stored_locally() {
    let w = world();
    w.publish(1, vec![w.host_plugin("me/a", "1.0.0", &[])]);
    assert!(w.market.my_rating("me/a").unwrap().is_none());
    assert_eq!(w.market.rate("me/a", 4).unwrap().score, 4);
    assert_eq!(w.market.rate("me/a", 5).unwrap().score, 5);
    assert_eq!(w.market.my_rating("me/a").unwrap().unwrap().score, 5);
    assert_eq!(
        w.market.rate("me/a", 0).unwrap_err().code(),
        "invalid_input"
    );
    assert_eq!(w.market.rate("me/none", 3).unwrap_err().code(), "not_found");
}

#[test]
fn versions_compare_numerically() {
    use std::cmp::Ordering::*;
    for (a, b, expected) in [
        ("1.10.0", "1.9.0", Greater),
        ("1.0", "1.0.0", Equal),
        ("v2.0.0", "2.0.0", Equal),
        ("1.0.0-beta", "1.0.0", Less),
        ("1.0.1", "1.0.0-rc1", Greater),
        ("1.0.0+build5", "1.0.0", Equal),
        ("0.9.9", "1.0.0", Less),
        ("1.0.0-alpha", "1.0.0-beta", Less),
    ] {
        assert_eq!(compare_versions(a, b), expected, "{a} vs {b}");
    }
}
