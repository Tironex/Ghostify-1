//! The whole application against a fake Spotify installation: patch it, then
//! talk to the bridge using only what the patched bundle contains.

use futures_util::{SinkExt, StreamExt};
use ghostify_app::{App, Core, CoreOptions, LOADER_JS};
use ghostify_core::{AppPaths, Config};
use ghostify_db::repo::market::ListingKind;
use ghostify_market::index::{Index, IndexEntry};
use ghostify_market::signed::{self, TrustedKey};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn make_spa(path: &Path) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    zip.start_file("index.html", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"<html><head><title>Spotify</title></head><body></body></html>")
        .unwrap();
    zip.finish().unwrap();
}

fn read_entry(path: &Path, name: &str) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut bytes = Vec::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn send(ws: &mut Ws, id: u64, method: &str, params: Value) {
    let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    ws.send(Message::text(request.to_string())).await.unwrap();
}

/// Reads frames until the response with `id` arrives.
async fn answer(ws: &mut Ws, id: u64) -> Value {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("answer in time")
            .unwrap()
            .unwrap();
        if let Message::Text(text) = frame {
            let value: Value = serde_json::from_str(text.as_str()).unwrap();
            if value["id"] == json!(id) {
                return value;
            }
        }
    }
}

async fn notification(ws: &mut Ws, method: &str) -> Value {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap_or_else(|_| panic!("no `{method}` notification"))
            .unwrap()
            .unwrap();
        if let Message::Text(text) = frame {
            let value: Value = serde_json::from_str(text.as_str()).unwrap();
            if value["method"] == method {
                return value["params"].clone();
            }
        }
    }
}

async fn connect(port: u64, secret: &str) -> Ws {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .unwrap();
    send(&mut ws, 1, "auth", json!({"secret": secret})).await;
    let reply = answer(&mut ws, 1).await;
    assert!(reply.get("error").is_none(), "{reply}");
    ws
}

#[tokio::test]
async fn patched_spotify_talks_to_the_running_app() {
    let tmp = tempfile::tempdir().unwrap();
    let apps = tmp.path().join("spotify/Apps");
    std::fs::create_dir_all(&apps).unwrap();
    make_spa(&apps.join("xpui.spa"));

    let port = free_port();
    let mut config = Config::default();
    config.bridge.port = port;
    config.patch.auto_repatch = false;
    let registry_key = signed::generate_key().unwrap();
    let mut options = CoreOptions::new(AppPaths::from_root(tmp.path().join("home")), config);
    options.market_keys = vec![TrustedKey {
        key_id: "main".into(),
        public_key: signed::encode_public_key(&registry_key),
    }];
    let apps_dir = apps.clone();
    options.detector = Some(std::sync::Arc::new(move || {
        ghostify_spotify::probe_custom(&apps_dir, None)
            .into_iter()
            .collect()
    }));
    let core = Core::open(options).unwrap();

    // Patch, then read the connection data from the bundle as the loader does.
    let installs = core.patch.detect_and_sync().await.unwrap();
    assert_eq!(installs.len(), 1);
    core.patch.patch(installs[0].id).await.unwrap();
    let spa = apps.join("xpui.spa");
    assert_eq!(read_entry(&spa, "ghostify/loader.js"), LOADER_JS);
    let script = String::from_utf8(read_entry(&spa, "ghostify/config.js")).unwrap();
    let json = script
        .trim()
        .strip_prefix("window.__GHOSTIFY__=")
        .and_then(|r| r.strip_suffix(';'))
        .unwrap();
    let loader: Value = serde_json::from_str(json).unwrap();
    assert_eq!(loader["ports"], json!([port]));
    let secret = loader["secret"].as_str().unwrap().to_owned();

    let (plugins, market) = (core.plugins.clone(), core.market.clone());
    let app = App::start(core).await.unwrap();
    assert_eq!(
        u64::from(app.bridge_port()),
        loader["ports"][0].as_u64().unwrap()
    );

    // A client without the secret is refused.
    let (mut stranger, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .unwrap();
    send(&mut stranger, 1, "auth", json!({"secret": "x".repeat(64)})).await;
    assert!(answer(&mut stranger, 1).await.get("error").is_some());

    let mut ws = connect(u64::from(port), &secret).await;
    send(
        &mut ws,
        2,
        "session.hello",
        json!({"client": "test", "version": "1"}),
    )
    .await;
    assert_eq!(answer(&mut ws, 2).await["result"]["server"], "ghostify");
    assert_eq!(app.connected_loaders(), 1);

    // Themes: activating pushes the stylesheet to the loader.
    let theme_src = tmp.path().join("theme-src");
    std::fs::create_dir_all(&theme_src).unwrap();
    std::fs::write(
        theme_src.join("theme.json"),
        r#"{"id":"me/dark","name":"Dark","version":"1.0.0","css":"user.css"}"#,
    )
    .unwrap();
    std::fs::write(theme_src.join("user.css"), "body { background: #101010; }").unwrap();
    app.themes
        .install_dir(
            theme_src,
            ghostify_db::repo::themes::ThemeSource::Local,
            false,
        )
        .await
        .unwrap();
    app.themes.activate("me/dark", None).await.unwrap();
    let pushed = notification(&mut ws, "theme.css").await;
    assert!(pushed.to_string().contains("#101010"), "{pushed}");
    send(&mut ws, 3, "theme.list", Value::Null).await;
    assert_eq!(answer(&mut ws, 3).await["result"][0]["id"], "me/dark");

    // Plugins: a change in the store reaches the loader.
    let plugin_src = tmp.path().join("plugin-src");
    std::fs::create_dir_all(&plugin_src).unwrap();
    std::fs::write(
        plugin_src.join("plugin.json"),
        r#"{"id":"me/hello","name":"Hello","version":"1.0.0"}"#,
    )
    .unwrap();
    std::fs::write(plugin_src.join("index.js"), "ghostify.ui.log('hi');").unwrap();
    let store = plugins.clone();
    tokio::task::spawn_blocking(move || {
        store
            .install_dir(
                &plugin_src,
                ghostify_plugins::store::InstallOptions::new(
                    ghostify_db::repo::plugins::PluginSource::Local,
                ),
            )
            .unwrap()
    })
    .await
    .unwrap();
    notification(&mut ws, "plugins.changed").await;

    // Marketplace: read-only methods work, installing is not offered to the renderer.
    let index = Index {
        sequence: 1,
        generated_at: 0,
        listings: vec![IndexEntry {
            id: "me/lyrics".into(),
            kind: ListingKind::Plugin,
            name: "Lyrics".into(),
            author: "me".into(),
            repo: "me/lyrics".into(),
            category: "lyrics".into(),
            description: "Shows lyrics".into(),
            tags: vec![],
            latest_version: "1.0.0".into(),
            stars: 1,
            rating_avg: None,
            rating_count: 0,
            download_url: "https://github.com/me/lyrics/releases/download/v1/lyrics.zip".into(),
            sha256: "ab".repeat(32),
            size: 10,
            permissions: vec![],
        }],
    };
    let text =
        serde_json::to_string(&signed::sign(&index, "main", &registry_key).unwrap()).unwrap();
    tokio::task::spawn_blocking(move || market.apply_index_text(&text).unwrap())
        .await
        .unwrap();
    send(
        &mut ws,
        4,
        "market.search",
        json!({"text": "lyr", "kind": null, "category": null, "limit": 10, "offset": 0}),
    )
    .await;
    assert_eq!(answer(&mut ws, 4).await["result"][0]["id"], "me/lyrics");
    send(&mut ws, 5, "market.categories", Value::Null).await;
    assert_eq!(answer(&mut ws, 5).await["result"][0]["category"], "lyrics");
    for (id, method) in [
        (6, "market.install"),
        (7, "plugin.install"),
        (8, "theme.install"),
    ] {
        send(&mut ws, id, method, json!({"id": "me/lyrics"})).await;
        assert_eq!(
            answer(&mut ws, id).await["error"]["code"],
            -32601,
            "{method} must not be exposed"
        );
    }

    app.shutdown().await;
    assert!(
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
        "the bridge port is released"
    );
}
