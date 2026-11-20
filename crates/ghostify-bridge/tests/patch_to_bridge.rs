//! Patch a bundle, then connect to the bridge using only what the patched bundle contains.

use futures_util::{SinkExt, StreamExt};
use ghostify_bridge::{secret, Bridge, BridgeConfig, Router};
use ghostify_db::Db;
use ghostify_patch::{engine, LoaderConfig, PatchSource, StaticSource};
use std::io::{Read, Write};
use std::path::Path;
use tokio_tungstenite::tungstenite::Message;

fn make_spa(path: &Path) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("index.html", options).unwrap();
    zip.write_all(b"<html><head><title>Spotify</title></head><body></body></html>")
        .unwrap();
    zip.finish().unwrap();
}

fn read_entry(path: &Path, name: &str) -> String {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut text = String::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

#[tokio::test]
async fn patched_bundle_carries_everything_the_loader_needs_to_reach_the_bridge() {
    let db = Db::open_in_memory().unwrap();
    let secret = secret::load_or_create(&db).unwrap();
    let mut config = BridgeConfig::new(secret.clone());
    config.ports = vec![0];
    let bridge = Bridge::start(config, Router::with_builtin(db))
        .await
        .unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let spa = tmp.path().join("xpui.spa");
    make_spa(&spa);
    let loader_config = LoaderConfig::new(vec![bridge.port()], secret, "0.1.0").unwrap();
    let source = StaticSource::builtin("/* loader */", "/* css */", &loader_config).unwrap();
    let (rules, assets) = source.load().unwrap();
    engine::apply(&spa, &rules, &assets, None).unwrap();

    let html = read_entry(&spa, "index.html");
    let config_at = html
        .find("ghostify/config.js")
        .expect("config.js is referenced");
    let loader_at = html
        .find("ghostify/loader.js")
        .expect("loader.js is referenced");
    assert!(
        config_at < loader_at,
        "config.js must run before the loader"
    );

    // Do what the loader does: read window.__GHOSTIFY__ from config.js.
    let script = read_entry(&spa, "ghostify/config.js");
    let json = script
        .trim()
        .strip_prefix("window.__GHOSTIFY__=")
        .and_then(|rest| rest.strip_suffix(';'))
        .expect("config.js has the expected shape");
    let value: serde_json::Value = serde_json::from_str(json).unwrap();
    let port = value["ports"][0].as_u64().unwrap();
    let secret = value["secret"].as_str().unwrap();

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .unwrap();
    let auth = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "auth", "params": {"secret": secret}});
    ws.send(Message::text(auth.to_string())).await.unwrap();
    let reply: serde_json::Value = match ws.next().await.unwrap().unwrap() {
        Message::Text(text) => serde_json::from_str(text.as_str()).unwrap(),
        other => panic!("unexpected frame {other:?}"),
    };
    assert!(reply["result"]["session_id"].is_u64(), "{reply}");
    assert_eq!(bridge.connected(), 1);
    bridge.shutdown().await;
}
