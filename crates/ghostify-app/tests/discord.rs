//! Discord presence end to end: loader report -> bridge -> hub -> Discord IPC socket.
//! One test per file because it sets `XDG_RUNTIME_DIR` for the whole process.

#![cfg(unix)]

use futures_util::{SinkExt, StreamExt};
use ghostify_app::{App, Core, CoreOptions};
use ghostify_core::{AppPaths, Config};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

fn read_frame(stream: &mut impl Read) -> std::io::Result<(u32, Value)> {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header)?;
    let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body)?;
    Ok((
        u32::from_le_bytes([header[0], header[1], header[2], header[3]]),
        serde_json::from_slice(&body).unwrap(),
    ))
}

fn write_frame(stream: &mut impl Write, opcode: u32, value: &Value) {
    let body = serde_json::to_vec(value).unwrap();
    stream.write_all(&opcode.to_le_bytes()).unwrap();
    stream
        .write_all(&(body.len() as u32).to_le_bytes())
        .unwrap();
    stream.write_all(&body).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_presence_reaches_discord_and_is_cleared_on_shutdown() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = tmp.path().join("run");
    std::fs::create_dir_all(&runtime).unwrap();
    std::env::set_var("XDG_RUNTIME_DIR", &runtime);

    let received: Arc<Mutex<Vec<Value>>> = Arc::default();
    let listener = UnixListener::bind(runtime.join("discord-ipc-0")).unwrap();
    let log = received.clone();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let Ok((_, hello)) = read_frame(&mut stream) else {
            return;
        };
        log.lock().unwrap().push(hello);
        write_frame(
            &mut stream,
            1,
            &json!({"cmd": "DISPATCH", "evt": "READY", "data": {}}),
        );
        while let Ok((_, request)) = read_frame(&mut stream) {
            let nonce = request["nonce"].clone();
            log.lock().unwrap().push(request);
            write_frame(
                &mut stream,
                1,
                &json!({"cmd": "SET_ACTIVITY", "evt": null, "nonce": nonce}),
            );
        }
    });

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut config = Config::default();
    config.bridge.port = port;
    config.patch.auto_repatch = false;
    config.extras.discord_enabled = true;
    config.extras.discord_client_id = "123456789012345678".into();
    let mut options = CoreOptions::new(AppPaths::from_root(tmp.path().join("home")), config);
    options.detector = Some(Arc::new(Vec::new));
    let core = Core::open(options).unwrap();
    let secret = core.bridge_secret.clone();
    let app = App::start(core).await.unwrap();

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .unwrap();
    ws.send(Message::text(
        json!({"jsonrpc": "2.0", "id": 1, "method": "auth", "params": {"secret": secret}})
            .to_string(),
    ))
    .await
    .unwrap();
    ws.next().await.unwrap().unwrap();
    let report = json!({"playing": true, "title": "Song", "artist": "Artist", "album": "Album", "position_ms": 0, "duration_ms": 180_000});
    ws.send(Message::text(
        json!({"jsonrpc": "2.0", "id": 2, "method": "player.report", "params": report}).to_string(),
    ))
    .await
    .unwrap();
    ws.next().await.unwrap().unwrap();

    let wait = |check: &dyn Fn(&[Value]) -> bool| {
        for _ in 0..300 {
            if check(&received.lock().unwrap()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out: {:?}", received.lock().unwrap());
    };
    wait(&|l| l.iter().any(|r| r["args"]["activity"]["details"] == "Song"));
    let log = received.lock().unwrap().clone();
    assert_eq!(log[0], json!({"v": 1, "client_id": "123456789012345678"}));
    // The first update after start clears a stale presence; the track follows.
    assert!(log[1]["args"]["activity"].is_null());
    let shown = log
        .iter()
        .find(|r| r["args"]["activity"]["details"] == "Song")
        .unwrap();
    assert_eq!(shown["args"]["activity"]["state"], "by Artist");

    app.shutdown().await;
    wait(&|l| l.last().is_some_and(|r| r["args"]["activity"].is_null()));
}
