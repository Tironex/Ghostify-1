//! Player state, commands, lyrics and hotkeys through the running application.

use futures_util::{SinkExt, StreamExt};
use ghostify_app::api::Api;
use ghostify_app::{App, Core, CoreOptions};
use ghostify_core::{AppPaths, Config};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn send(ws: &mut Ws, id: u64, method: &str, params: Value) {
    ws.send(Message::text(
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
    ))
    .await
    .unwrap();
}

async fn next_json(ws: &mut Ws) -> Value {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("message in time")
            .unwrap()
            .unwrap();
        if let Message::Text(text) = frame {
            return serde_json::from_str(text.as_str()).unwrap();
        }
    }
}

async fn answer(ws: &mut Ws, id: u64) -> Value {
    loop {
        let value = next_json(ws).await;
        if value["id"] == json!(id) {
            return value;
        }
    }
}

struct Fixture {
    api: Api,
    ws: Ws,
    lyrics_dir: std::path::PathBuf,
    _app: Arc<App>,
    _tmp: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut config = Config::default();
    config.bridge.port = port;
    config.patch.auto_repatch = false;
    let mut options = CoreOptions::new(AppPaths::from_root(tmp.path().join("home")), config);
    options.detector = Some(Arc::new(Vec::new));
    options.lyrics_fetcher = None;
    let core = Core::open(options).unwrap();
    let secret = core.bridge_secret.clone();
    let lyrics_dir = core.lyrics.dir().to_owned();
    let app = Arc::new(App::start(core).await.unwrap());

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .unwrap();
    send(&mut ws, 1, "auth", json!({"secret": secret})).await;
    assert!(answer(&mut ws, 1).await.get("error").is_none());
    Fixture {
        api: Api::new(app.clone()),
        ws,
        lyrics_dir,
        _app: app,
        _tmp: tmp,
    }
}

fn report(title: &str, playing: bool, position: u64) -> Value {
    json!({"playing": playing, "title": title, "artist": "Artist", "album": "Album", "position_ms": position, "duration_ms": 200_000})
}

#[tokio::test]
async fn the_loader_reports_and_the_interface_sends_commands() {
    let mut f = fixture().await;
    assert_eq!(
        f.api.call("player.state", Value::Null).await.unwrap()["track"],
        Value::Null
    );

    send(&mut f.ws, 2, "player.report", report("Song", true, 12_000)).await;
    assert!(answer(&mut f.ws, 2).await.get("error").is_none());
    let state = f.api.call("player.state", Value::Null).await.unwrap();
    assert_eq!(state["track"]["title"], "Song");
    assert_eq!(state["playing"], true);
    assert!(state["position_ms"].as_u64().unwrap() >= 12_000);

    // A report with a bad payload is rejected and leaves the state alone.
    send(
        &mut f.ws,
        3,
        "player.report",
        json!({"playing": true, "title": "x", "artwork": "http://evil"}),
    )
    .await;
    assert_eq!(answer(&mut f.ws, 3).await["error"]["code"], -32602);
    assert_eq!(
        f.api.call("player.state", Value::Null).await.unwrap()["track"]["title"],
        "Song"
    );

    f.api
        .call("player.command", json!({"action": "next"}))
        .await
        .unwrap();
    let pushed = loop {
        let message = next_json(&mut f.ws).await;
        if message["method"] == "player.command" {
            break message;
        }
    };
    assert_eq!(pushed["params"], json!({"action": "next"}));
    assert_eq!(
        f.api
            .call("player.command", json!({"action": "explode"}))
            .await
            .unwrap_err()
            .code(),
        "invalid_input"
    );
}

#[tokio::test]
async fn commands_fail_clearly_without_spotify() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.bridge.port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    config.patch.auto_repatch = false;
    let mut options = CoreOptions::new(AppPaths::from_root(tmp.path().join("home")), config);
    options.detector = Some(Arc::new(Vec::new));
    let app = Arc::new(App::start(Core::open(options).unwrap()).await.unwrap());
    let api = Api::new(app);
    let err = api
        .call("player.command", json!({"action": "toggle"}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found");
    assert!(err.to_string().contains("not connected"));
    assert_eq!(
        api.call("hotkeys.trigger", json!({"action": "play_pause"}))
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
}

#[tokio::test]
async fn lyrics_come_from_local_files_for_the_current_track() {
    let mut f = fixture().await;
    assert_eq!(
        f.api.call("lyrics.current", Value::Null).await.unwrap(),
        json!({"track": null, "lyrics": null})
    );

    send(&mut f.ws, 2, "player.report", report("Song", true, 0)).await;
    answer(&mut f.ws, 2).await;
    assert_eq!(
        f.api.call("lyrics.current", Value::Null).await.unwrap()["lyrics"],
        Value::Null
    );

    std::fs::write(
        f.lyrics_dir.join("Artist - Song.lrc"),
        "[00:01.00]First line\n[00:05.00]Second line",
    )
    .unwrap();
    let current = f.api.call("lyrics.current", Value::Null).await.unwrap();
    assert_eq!(current["track"]["title"], "Song");
    assert_eq!(current["lyrics"]["source"], "local");
    assert_eq!(
        current["lyrics"]["synced"][1],
        json!({"time_ms": 5000, "text": "Second line"})
    );
    let info = f.api.call("app.info", Value::Null).await.unwrap();
    assert_eq!(info["lyrics_dir"], json!(f.lyrics_dir));
}

#[tokio::test]
async fn hotkeys_can_be_listed_changed_and_triggered() {
    let mut f = fixture().await;
    let list = f.api.call("hotkeys.list", Value::Null).await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 4);

    let changed = f
        .api
        .call(
            "hotkeys.set",
            json!({"action": "next", "accelerator": "ctrl+alt+n"}),
        )
        .await
        .unwrap();
    assert_eq!(changed["accelerator"], "Control+Alt+N");
    assert_eq!(
        f.api
            .call("hotkeys.set", json!({"action": "next", "accelerator": "N"}))
            .await
            .unwrap_err()
            .code(),
        "invalid_input"
    );
    assert_eq!(
        f.api
            .call(
                "hotkeys.set",
                json!({"action": "previous", "accelerator": "Control+Alt+N"})
            )
            .await
            .unwrap_err()
            .code(),
        "conflict"
    );
    assert_eq!(
        f.api
            .call("hotkeys.reset", json!({"action": "next"}))
            .await
            .unwrap()["accelerator"],
        "CommandOrControl+Alt+Right"
    );

    assert_eq!(
        f.api
            .call("hotkeys.trigger", json!({"action": "toggle_mini_player"}))
            .await
            .unwrap(),
        json!({"ui": "toggle_mini_player"})
    );
    send(&mut f.ws, 2, "player.report", report("Song", true, 0)).await;
    answer(&mut f.ws, 2).await;
    f.api
        .call("hotkeys.trigger", json!({"action": "play_pause"}))
        .await
        .unwrap();
    loop {
        let message = next_json(&mut f.ws).await;
        if message["method"] == "player.command" {
            assert_eq!(message["params"], json!({"action": "toggle"}));
            break;
        }
    }
    assert_eq!(
        f.api
            .call("hotkeys.trigger", json!({"action": "nope"}))
            .await
            .unwrap_err()
            .code(),
        "invalid_input"
    );
}
