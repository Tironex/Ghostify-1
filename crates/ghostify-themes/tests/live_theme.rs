//! Theme changes reach a connected loader through the real bridge.

use futures_util::{SinkExt, StreamExt};
use ghostify_bridge::{secret, Bridge, BridgeConfig, Router};
use ghostify_core::AppPaths;
use ghostify_db::{repo::themes::ThemeSource, Db};
use ghostify_themes::{ThemeService, ThemeStore};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_json(ws: &mut Client) -> Value {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timeout")
            .expect("closed")
            .expect("error");
        if let Message::Text(text) = frame {
            return serde_json::from_str(text.as_str()).unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn loader_receives_current_theme_and_live_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::from_root(tmp.path());
    let db = Db::open_in_memory().unwrap();
    let store = ThemeStore::new(db.clone(), &paths).unwrap();

    // The bridge needs the router before it starts, the service needs the bridge to notify.
    let slot: Arc<std::sync::OnceLock<Arc<Bridge>>> = Arc::default();
    let sink = slot.clone();
    let service = ThemeService::new(
        store,
        Arc::new(move |method, params| {
            if let Some(bridge) = sink.get() {
                bridge.notify(method, params);
            }
        }),
        Duration::from_millis(80),
    );
    let mut router = Router::with_builtin(db.clone());
    service.register(&mut router);
    let secret = secret::load_or_create(&db).unwrap();
    let mut config = BridgeConfig::new(secret.clone());
    config.ports = vec![0];
    let bridge = Arc::new(Bridge::start(config, router).await.unwrap());
    slot.set(bridge.clone()).ok();

    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("theme.json"),
        r#"{"id":"me/x","name":"X","version":"1"}"#,
    )
    .unwrap();
    std::fs::write(src.join("user.css"), "v1{}").unwrap();
    service
        .install_dir(src, ThemeSource::Local, false)
        .await
        .unwrap();
    service.activate("me/x", None).await.unwrap();

    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/", bridge.port()))
            .await
            .unwrap();
    ws.send(Message::text(
        json!({"jsonrpc":"2.0","id":1,"method":"auth","params":{"secret":secret}}).to_string(),
    ))
    .await
    .unwrap();
    assert!(next_json(&mut ws).await["result"].is_object());
    ws.send(Message::text(
        json!({"jsonrpc":"2.0","id":2,"method":"theme.current"}).to_string(),
    ))
    .await
    .unwrap();
    let current = next_json(&mut ws).await;
    assert_eq!(current["result"]["css"], "v1{}");

    std::fs::write(
        service.store().theme_dir("me/x").unwrap().join("user.css"),
        "v2{}",
    )
    .unwrap();
    let push = next_json(&mut ws).await;
    assert_eq!(push["method"], "theme.css");
    assert_eq!(push["params"]["css"], "v2{}");

    // The loader cannot change themes.
    ws.send(Message::text(
        json!({"jsonrpc":"2.0","id":3,"method":"theme.activate","params":{"id":"me/x"}})
            .to_string(),
    ))
    .await
    .unwrap();
    // Further live-reload pushes may still be in flight; skip them and look for the answer.
    let answer = loop {
        let message = next_json(&mut ws).await;
        if message["id"] == 3 {
            break message;
        }
        assert!(
            matches!(
                message["method"].as_str(),
                Some("theme.css" | "theme.scheme")
            ),
            "{message}"
        );
    };
    assert_eq!(answer["error"]["code"], -32601);
}
