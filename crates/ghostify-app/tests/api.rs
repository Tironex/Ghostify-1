//! The interface's calls against a fake Spotify installation.

use ghostify_app::api::Api;
use ghostify_app::{App, Core, CoreOptions};
use ghostify_core::{AppPaths, Config};
use serde_json::{json, Value};
use std::io::Write;
use std::sync::Arc;

struct Fixture {
    api: Api,
    app: Arc<App>,
    tmp: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let apps = tmp.path().join("spotify/Apps");
    std::fs::create_dir_all(&apps).unwrap();
    let mut zip = zip::ZipWriter::new(std::fs::File::create(apps.join("xpui.spa")).unwrap());
    zip.start_file("index.html", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"<html><head></head><body></body></html>")
        .unwrap();
    zip.finish().unwrap();

    let mut config = Config::default();
    config.bridge.port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    config.patch.auto_repatch = false;
    let mut options = CoreOptions::new(AppPaths::from_root(tmp.path().join("home")), config);
    options.detector = Some(Arc::new(move || {
        ghostify_spotify::probe_custom(&apps, None)
            .into_iter()
            .collect()
    }));
    let app = Arc::new(App::start(Core::open(options).unwrap()).await.unwrap());
    Fixture {
        api: Api::new(app.clone()),
        app,
        tmp,
    }
}

fn write(dir: &std::path::Path, name: &str, content: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), content).unwrap();
}

#[tokio::test]
async fn info_patch_and_errors() {
    let f = fixture().await;
    let info = f.api.call("app.info", Value::Null).await.unwrap();
    assert_eq!(info["bridge_port"], u64::from(f.app.bridge_port()));

    let status = f.api.call("patch.status", Value::Null).await.unwrap();
    assert_eq!(status[0]["disk"]["state"], "unpatched");
    let applied = f.api.call("patch.apply", Value::Null).await.unwrap();
    assert_eq!(applied[0]["outcome"], "patched");
    assert_eq!(
        f.api.call("patch.status", Value::Null).await.unwrap()[0]["disk"]["state"],
        "patched"
    );
    let repaired = f.api.call("patch.repair", Value::Null).await.unwrap();
    assert_eq!(repaired[0]["report"]["outcome"], "already_healthy");
    let restored = f.api.call("patch.restore", Value::Null).await.unwrap();
    assert_eq!(restored[0]["outcome"], "restored");
    assert_eq!(
        f.api
            .call("patch.apply", json!({"install": 999}))
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );

    assert_eq!(
        f.api
            .call("nope.nothing", Value::Null)
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        f.api
            .call("theme.activate", json!({"id": 5}))
            .await
            .unwrap_err()
            .code(),
        "invalid_input"
    );
    assert_eq!(
        f.api
            .call("theme.activate", Value::Null)
            .await
            .unwrap_err()
            .code(),
        "invalid_input"
    );
}

#[tokio::test]
async fn themes_end_to_end() {
    let f = fixture().await;
    let src = f.tmp.path().join("theme");
    write(
        &src,
        "theme.json",
        r#"{"id":"me/dark","name":"Dark","version":"1.0.0"}"#,
    );
    write(&src, "user.css", "body { background: #111; }");

    let installed = f
        .api
        .call("theme.install", json!({"path": src}))
        .await
        .unwrap();
    assert_eq!(installed["info"]["id"], "me/dark");
    assert_eq!(
        f.api
            .call("theme.install", json!({"path": src}))
            .await
            .unwrap_err()
            .code(),
        "conflict"
    );
    f.api
        .call("theme.install", json!({"path": src, "replace": true}))
        .await
        .unwrap();

    assert_eq!(
        f.api
            .call("theme.activate", json!({"id": "me/dark"}))
            .await
            .unwrap()["active"],
        true
    );
    assert!(
        f.api.call("theme.current", Value::Null).await.unwrap()["css"]
            .as_str()
            .unwrap()
            .contains("#111")
    );
    f.api
        .call("theme.set_mode", json!({"mode": "light"}))
        .await
        .unwrap();
    assert_eq!(
        f.api.call("theme.mode", Value::Null).await.unwrap(),
        "light"
    );
    assert_eq!(
        f.api
            .call("theme.set_mode", json!({"mode": "purple"}))
            .await
            .unwrap_err()
            .code(),
        "invalid_input"
    );

    let out = f.tmp.path().join("dark.zip");
    f.api
        .call("theme.export", json!({"id": "me/dark", "out": out}))
        .await
        .unwrap();
    assert!(out.metadata().unwrap().len() > 0);
    f.api
        .call("theme.remove", json!({"id": "me/dark"}))
        .await
        .unwrap();
    // Only the bundled Ghost theme is left.
    let left = f.api.call("theme.list", Value::Null).await.unwrap();
    let ids: Vec<&str> = left
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["ghostify/ghost"]);
    // The exported archive installs again.
    assert_eq!(
        f.api
            .call("theme.install", json!({"path": out}))
            .await
            .unwrap()["info"]["id"],
        "me/dark"
    );
    f.api.call("theme.deactivate", Value::Null).await.unwrap();
}

#[tokio::test]
async fn plugins_end_to_end() {
    let f = fixture().await;
    let src = f.tmp.path().join("plugin");
    write(
        &src,
        "plugin.json",
        r#"{"id":"me/hello","name":"Hello","version":"1.0.0","permissions":["storage"]}"#,
    );
    write(&src, "index.js", "ghostify.ui.log('hi');");

    let installed = f
        .api
        .call("plugin.install", json!({"path": src}))
        .await
        .unwrap();
    assert_eq!(installed["info"]["id"], "me/hello");
    assert_eq!(installed["report"]["verdict"], "clean");
    assert_eq!(installed["info"]["requested"], json!(["storage"]));

    let granted = f
        .api
        .call(
            "plugin.grant",
            json!({"id": "me/hello", "permission": "storage"}),
        )
        .await
        .unwrap();
    assert_eq!(granted["granted"], json!(["storage"]));
    assert_eq!(
        f.api
            .call(
                "plugin.grant",
                json!({"id": "me/hello", "permission": "root"})
            )
            .await
            .unwrap_err()
            .code(),
        "invalid_input"
    );
    assert_eq!(
        f.api
            .call("plugin.enable", json!({"id": "me/hello"}))
            .await
            .unwrap()["enabled"],
        true
    );
    assert_eq!(
        f.api
            .call("plugin.disable", json!({"id": "me/hello"}))
            .await
            .unwrap()["enabled"],
        false
    );
    assert_eq!(
        f.api
            .call(
                "plugin.revoke",
                json!({"id": "me/hello", "permission": "storage"})
            )
            .await
            .unwrap()["granted"],
        json!([])
    );
    assert_eq!(
        f.api
            .call("plugin.list", Value::Null)
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    f.api
        .call("plugin.remove", json!({"id": "me/hello"}))
        .await
        .unwrap();
    assert_eq!(
        f.api
            .call("plugin.info", json!({"id": "me/hello"}))
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );

    // A blocked plugin is refused unless the user decides otherwise.
    write(&src, "index.js", "eval(atob('YWxlcnQoMSk='));");
    write(
        &src,
        "plugin.json",
        r#"{"id":"me/bad","name":"Bad","version":"1.0.0"}"#,
    );
    assert!(f
        .api
        .call("plugin.install", json!({"path": src}))
        .await
        .is_err());
}

#[tokio::test]
async fn marketplace_calls_and_settings() {
    let f = fixture().await;
    assert_eq!(
        f.api
            .call(
                "market.search",
                json!({"text": "", "kind": null, "category": null, "limit": 10, "offset": 0})
            )
            .await
            .unwrap(),
        json!([])
    );
    assert_eq!(
        f.api
            .call("market.get", json!({"id": "me/x"}))
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        f.api.call("market.updates", Value::Null).await.unwrap(),
        json!([])
    );
    assert_eq!(
        f.api
            .call("market.rate", json!({"id": "me/x", "score": 5}))
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        f.api
            .call("market.my_rating", json!({"id": "me/x"}))
            .await
            .unwrap(),
        Value::Null
    );

    let config = f.api.call("config.get", Value::Null).await.unwrap();
    assert_eq!(config["plugins"]["auto_update"], "safe_only");
    let mut edited = config.clone();
    edited["plugins"]["auto_update"] = json!("off");
    assert_eq!(
        f.api
            .call("config.save", json!({"config": edited}))
            .await
            .unwrap()["restart_required"],
        true
    );
    let saved = Config::load(&f.app.core.paths.config_file()).unwrap();
    assert_eq!(
        saved.plugins.auto_update,
        ghostify_core::config::AutoUpdate::Off
    );

    let mut invalid = config;
    invalid["marketplace"]["index_url"] = json!("http://insecure.example/i.json");
    assert!(f
        .api
        .call("config.save", json!({"config": invalid}))
        .await
        .is_err());
}
