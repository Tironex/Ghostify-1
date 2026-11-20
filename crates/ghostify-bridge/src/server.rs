//! WebSocket server on the loopback interface.
//!
//! A connection must pass four gates before it may call anything:
//! 1. the TCP peer is loopback (the listener only binds `127.0.0.1`),
//! 2. the `Host` header names a loopback host (defeats DNS rebinding),
//! 3. the `Origin` header, if present, is on the allow-list (defeats other web pages),
//! 4. the first message authenticates with the per-installation secret.

use crate::protocol::{
    notification_text, Request, Response, RpcError, INVALID_REQUEST, PARSE_ERROR, RATE_LIMITED,
    UNAUTHORIZED,
};
use crate::router::{CallContext, Router};
use crate::secret::constant_time_eq;
use futures_util::{SinkExt, StreamExt};
use ghostify_core::{Error, Result};
use serde_json::Value;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch, Semaphore};
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::handshake::server::{
    ErrorResponse, Request as HttpRequest, Response as HttpResponse,
};
use tokio_tungstenite::tungstenite::http::{self, StatusCode};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

/// Origins the Spotify renderer uses. Requests without an `Origin` header are
/// native clients and are allowed because they still need the secret.
pub const DEFAULT_ALLOWED_ORIGINS: &[&str] = &[
    "https://xpui.app.spotify.com",
    "spotify://xpui.app.spotify.com",
];

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Ports tried in order; the first free one wins. `0` lets the OS choose.
    pub ports: Vec<u16>,
    pub secret: String,
    pub allowed_origins: Vec<String>,
    pub max_message_bytes: usize,
    /// Messages per second and connection before requests are refused.
    pub rate_limit_per_sec: u32,
    pub auth_timeout: Duration,
    /// Requests of one connection that may run at the same time.
    pub max_in_flight: usize,
}

impl BridgeConfig {
    /// Defaults: ports 49421-49430, 1 MiB messages, 200 messages/s, 5 s to authenticate.
    pub fn new(secret: impl Into<String>) -> BridgeConfig {
        BridgeConfig {
            ports: (49421..=49430).collect(),
            secret: secret.into(),
            allowed_origins: DEFAULT_ALLOWED_ORIGINS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            max_message_bytes: 1024 * 1024,
            rate_limit_per_sec: 200,
            auth_timeout: Duration::from_secs(5),
            max_in_flight: 32,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.secret.len() < 32 {
            return Err(Error::Invalid(
                "the bridge secret must have at least 32 characters".into(),
            ));
        }
        if self.ports.is_empty() {
            return Err(Error::Invalid("no bridge port configured".into()));
        }
        if self.max_message_bytes < 1024 || self.rate_limit_per_sec == 0 || self.max_in_flight == 0
        {
            return Err(Error::Invalid("bridge limits are too small".into()));
        }
        Ok(())
    }
}

struct Shared {
    config: BridgeConfig,
    router: Router,
    notify: broadcast::Sender<Arc<str>>,
    sessions: AtomicU64,
    connected: AtomicUsize,
}

/// A running bridge. Dropping it does not stop the server; call [`Bridge::shutdown`].
pub struct Bridge {
    addr: SocketAddr,
    shared: Arc<Shared>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl Bridge {
    pub async fn start(config: BridgeConfig, router: Router) -> Result<Bridge> {
        config.validate()?;
        let listener = bind(&config.ports).await?;
        let addr = listener.local_addr()?;
        let (notify, _) = broadcast::channel(256);
        let shared = Arc::new(Shared {
            config,
            router,
            notify,
            sessions: AtomicU64::new(0),
            connected: AtomicUsize::new(0),
        });
        let (stop, stop_rx) = watch::channel(false);
        let task = tokio::spawn(accept_loop(listener, shared.clone(), stop_rx));
        tracing::info!(%addr, "bridge listening");
        Ok(Bridge {
            addr,
            shared,
            stop,
            task,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Number of authenticated connections.
    pub fn connected(&self) -> usize {
        self.shared.connected.load(Ordering::SeqCst)
    }

    /// Pushes a notification to every authenticated connection. Returns how
    /// many connections were subscribed at that moment.
    pub fn notify(&self, method: &str, params: &Value) -> usize {
        let text: Arc<str> = notification_text(method, params).into();
        self.shared.notify.send(text).unwrap_or(0)
    }

    /// Closes all connections and waits until the server tasks are gone.
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

async fn bind(ports: &[u16]) -> Result<TcpListener> {
    let mut last = None;
    for port in ports {
        match TcpListener::bind((Ipv4Addr::LOCALHOST, *port)).await {
            Ok(listener) => return Ok(listener),
            Err(err) => last = Some(err),
        }
    }
    Err(Error::Io(
        last.unwrap_or_else(|| std::io::Error::other("no port")),
    ))
}

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    if !peer.ip().is_loopback() {
                        continue;
                    }
                    connections.spawn(handle_connection(stream, shared.clone(), stop.clone()));
                }
                Err(err) => {
                    tracing::warn!(%err, "accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    drop(listener);
    while connections.join_next().await.is_some() {}
}

fn reject(status: StatusCode, reason: &str) -> ErrorResponse {
    let mut response = http::Response::new(Some(reason.to_owned()));
    *response.status_mut() = status;
    response
}

fn host_is_loopback(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => host.rsplit_once(':').map_or(host, |(name, _)| name),
    };
    matches!(name, "127.0.0.1" | "localhost" | "::1")
}

/// Handshake gate: Host and Origin checks. The error type is dictated by tungstenite's `Callback`.
#[allow(clippy::result_large_err)]
fn check_handshake(
    config: &BridgeConfig,
    request: &HttpRequest,
) -> std::result::Result<(), ErrorResponse> {
    let host = request.headers().get("host").and_then(|v| v.to_str().ok());
    if !host.is_some_and(host_is_loopback) {
        return Err(reject(StatusCode::FORBIDDEN, "bad host"));
    }
    if let Some(origin) = request.headers().get("origin") {
        let allowed = origin
            .to_str()
            .map(|o| config.allowed_origins.iter().any(|a| a == o))
            .unwrap_or(false);
        if !allowed {
            return Err(reject(StatusCode::FORBIDDEN, "origin not allowed"));
        }
    }
    Ok(())
}

enum Out {
    Text(String),
    Close,
}

#[allow(clippy::result_large_err)]
async fn handle_connection(
    stream: TcpStream,
    shared: Arc<Shared>,
    mut stop: watch::Receiver<bool>,
) {
    let _ = stream.set_nodelay(true);
    let max = shared.config.max_message_bytes;
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(max))
        .max_frame_size(Some(max));
    let config = shared.config.clone();
    let handshake = tokio_tungstenite::accept_hdr_async_with_config(
        stream,
        move |request: &HttpRequest, response: HttpResponse| {
            check_handshake(&config, request).map(|()| response)
        },
        Some(ws_config),
    );
    let mut ws = match timeout(shared.config.auth_timeout, handshake).await {
        Ok(Ok(ws)) => ws,
        Ok(Err(err)) => {
            tracing::debug!(%err, "bridge handshake rejected");
            return;
        }
        Err(_) => return,
    };

    // Authentication: the very first message.
    let session_id = match timeout(shared.config.auth_timeout, authenticate(&mut ws, &shared)).await
    {
        Ok(Some(id)) => id,
        _ => {
            let _ = ws.close(None).await;
            return;
        }
    };
    shared.connected.fetch_add(1, Ordering::SeqCst);
    run_session(ws, &shared, session_id, &mut stop).await;
    shared.connected.fetch_sub(1, Ordering::SeqCst);
}

type Ws = tokio_tungstenite::WebSocketStream<TcpStream>;

async fn authenticate(ws: &mut Ws, shared: &Shared) -> Option<u64> {
    let message = loop {
        match ws.next().await? {
            Ok(Message::Text(text)) => break text,
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            _ => return None,
        }
    };
    let request: Request = serde_json::from_str(message.as_str()).ok()?;
    let id = request.id.clone().unwrap_or(Value::Null);
    let supplied = request
        .params
        .get("secret")
        .and_then(Value::as_str)
        .unwrap_or("");
    let valid = request.method == "auth" && constant_time_eq(supplied, &shared.config.secret);
    if !valid {
        let response = Response::err(id, RpcError::new(UNAUTHORIZED, "authentication required"));
        let _ = ws.send(Message::text(response.to_text())).await;
        return None;
    }
    let session_id = shared.sessions.fetch_add(1, Ordering::SeqCst) + 1;
    let response = Response::ok(id, serde_json::json!({ "session_id": session_id }));
    ws.send(Message::text(response.to_text())).await.ok()?;
    Some(session_id)
}

/// Fixed one-second window counter.
struct RateLimiter {
    limit: u32,
    window_start: Instant,
    count: u32,
}

impl RateLimiter {
    fn new(limit: u32) -> RateLimiter {
        RateLimiter {
            limit,
            window_start: Instant::now(),
            count: 0,
        }
    }

    fn allow(&mut self) -> bool {
        if self.window_start.elapsed() >= Duration::from_secs(1) {
            self.window_start = Instant::now();
            self.count = 0;
        }
        self.count += 1;
        self.count <= self.limit
    }
}

async fn run_session(
    ws: Ws,
    shared: &Arc<Shared>,
    session_id: u64,
    stop: &mut watch::Receiver<bool>,
) {
    let (mut sink, mut stream) = ws.split();
    let (out_tx, mut out_rx) = mpsc::channel::<Out>(256);
    let mut pushes = shared.notify.subscribe();

    let writer = tokio::spawn(async move {
        loop {
            let message = tokio::select! {
                out = out_rx.recv() => match out {
                    Some(Out::Text(text)) => Message::text(text),
                    Some(Out::Close) | None => break,
                },
                push = pushes.recv() => match push {
                    Ok(text) => Message::text(text.to_string()),
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "bridge client too slow, notifications dropped");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            };
            if sink.send(message).await.is_err() {
                return;
            }
        }
        let _ = sink.send(Message::Close(None)).await;
        let _ = sink.close().await;
    });

    let in_flight = Arc::new(Semaphore::new(shared.config.max_in_flight));
    let mut limiter = RateLimiter::new(shared.config.rate_limit_per_sec);
    let ctx = CallContext { session_id };
    let mut tasks = JoinSet::new();

    loop {
        let item = tokio::select! {
            _ = stop.changed() => break,
            item = stream.next() => item,
            Some(_) = tasks.join_next(), if !tasks.is_empty() => continue,
        };
        let text = match item {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(Message::Close(_))) | None => break,
            Some(Ok(_)) => {
                let response = Response::err(
                    Value::Null,
                    RpcError::new(INVALID_REQUEST, "only text frames are supported"),
                );
                let _ = out_tx.send(Out::Text(response.to_text())).await;
                continue;
            }
            // Oversized or malformed frames end the connection.
            Some(Err(err)) => {
                tracing::debug!(%err, session_id, "bridge connection error");
                break;
            }
        };

        let request: Request = match serde_json::from_str(text.as_str()) {
            Ok(request) => request,
            Err(err) => {
                let (code, message) = if serde_json::from_str::<Value>(text.as_str()).is_ok() {
                    (INVALID_REQUEST, "not a JSON-RPC request".to_owned())
                } else {
                    (PARSE_ERROR, err.to_string())
                };
                let response = Response::err(Value::Null, RpcError::new(code, message));
                let _ = out_tx.send(Out::Text(response.to_text())).await;
                continue;
            }
        };
        if request.jsonrpc != "2.0" {
            let id = request.id.unwrap_or(Value::Null);
            let response = Response::err(
                id,
                RpcError::new(INVALID_REQUEST, "jsonrpc must be \"2.0\""),
            );
            let _ = out_tx.send(Out::Text(response.to_text())).await;
            continue;
        }
        if !limiter.allow() {
            if let Some(id) = request.id {
                let response = Response::err(id, RpcError::new(RATE_LIMITED, "too many requests"));
                let _ = out_tx.send(Out::Text(response.to_text())).await;
            }
            continue;
        }

        // Wait for a slot here so a flood backs up into TCP instead of memory.
        let Ok(permit) = in_flight.clone().acquire_owned().await else {
            break;
        };
        let shared = shared.clone();
        let out = out_tx.clone();
        tasks.spawn(async move {
            let _permit = permit;
            let result = shared
                .router
                .dispatch(ctx, &request.method, request.params)
                .await;
            if let Some(id) = request.id {
                let response = match result {
                    Ok(value) => Response::ok(id, value),
                    Err(error) => Response::err(id, error),
                };
                let _ = out.send(Out::Text(response.to_text())).await;
            }
        });
    }

    tasks.abort_all();
    let _ = out_tx.send(Out::Close).await;
    drop(out_tx);
    let _ = timeout(Duration::from_secs(2), writer).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{METHOD_NOT_FOUND, RATE_LIMITED};
    use crate::secret;
    use ghostify_db::Db;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    type Client = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

    async fn start_with(edit: impl FnOnce(&mut BridgeConfig), router: Router) -> (Bridge, String) {
        let secret = secret::generate().unwrap();
        let mut config = BridgeConfig::new(secret.clone());
        config.ports = vec![0];
        config.auth_timeout = Duration::from_secs(2);
        edit(&mut config);
        (Bridge::start(config, router).await.unwrap(), secret)
    }

    async fn start() -> (Bridge, String) {
        start_with(|_| {}, Router::with_builtin(Db::open_in_memory().unwrap())).await
    }

    async fn connect_with(
        bridge: &Bridge,
        host: Option<&str>,
        origin: Option<&str>,
    ) -> std::result::Result<Client, tokio_tungstenite::tungstenite::Error> {
        let mut request = format!("ws://127.0.0.1:{}/", bridge.port())
            .into_client_request()
            .unwrap();
        if let Some(host) = host {
            request.headers_mut().insert("host", host.parse().unwrap());
        }
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert("origin", origin.parse().unwrap());
        }
        tokio_tungstenite::connect_async(request)
            .await
            .map(|(ws, _)| ws)
    }

    async fn call(ws: &mut Client, id: u64, method: &str, params: Value) -> Value {
        let request =
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        ws.send(Message::text(request.to_string())).await.unwrap();
        recv(ws).await
    }

    async fn recv(ws: &mut Client) -> Value {
        loop {
            let next = timeout(Duration::from_secs(5), ws.next())
                .await
                .expect("timed out")
                .expect("closed");
            if let Message::Text(text) = next.expect("ws error") {
                return serde_json::from_str(text.as_str()).unwrap();
            }
        }
    }

    async fn authed(bridge: &Bridge, secret: &str) -> Client {
        let mut ws = connect_with(bridge, None, None).await.unwrap();
        let reply = call(&mut ws, 0, "auth", serde_json::json!({ "secret": secret })).await;
        assert!(reply["result"]["session_id"].is_u64(), "{reply}");
        ws
    }

    async fn is_closed(ws: &mut Client) -> bool {
        loop {
            match timeout(Duration::from_secs(5), ws.next()).await {
                Ok(None | Some(Err(_)) | Some(Ok(Message::Close(_)))) => return true,
                Ok(Some(Ok(_))) => continue,
                Err(_) => return false,
            }
        }
    }

    #[tokio::test]
    async fn authenticated_client_can_call_methods() {
        let (bridge, secret) = start().await;
        let mut ws = authed(&bridge, &secret).await;
        assert_eq!(
            call(&mut ws, 1, "ping", Value::Null).await["result"],
            "pong"
        );
        let unknown = call(&mut ws, 2, "nope", Value::Null).await;
        assert_eq!(unknown["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(unknown["id"], 2);
        assert_eq!(bridge.connected(), 1);
        bridge.shutdown().await;
    }

    #[tokio::test]
    async fn wrong_secret_is_refused_and_connection_closed() {
        let (bridge, _) = start().await;
        let mut ws = connect_with(&bridge, None, None).await.unwrap();
        let reply = call(
            &mut ws,
            1,
            "auth",
            serde_json::json!({"secret": "x".repeat(64)}),
        )
        .await;
        assert_eq!(reply["error"]["code"], UNAUTHORIZED);
        assert!(is_closed(&mut ws).await);
        assert_eq!(bridge.connected(), 0);
        bridge.shutdown().await;
    }

    #[tokio::test]
    async fn calling_before_auth_is_refused() {
        let (bridge, _) = start().await;
        let mut ws = connect_with(&bridge, None, None).await.unwrap();
        let reply = call(&mut ws, 1, "ping", Value::Null).await;
        assert_eq!(reply["error"]["code"], UNAUTHORIZED);
        assert!(is_closed(&mut ws).await);
        bridge.shutdown().await;
    }

    #[tokio::test]
    async fn silent_client_is_dropped_after_auth_timeout() {
        let (bridge, _) = start_with(
            |c| c.auth_timeout = Duration::from_millis(200),
            Router::new(),
        )
        .await;
        let mut ws = connect_with(&bridge, None, None).await.unwrap();
        assert!(is_closed(&mut ws).await);
        bridge.shutdown().await;
    }

    #[tokio::test]
    async fn foreign_origin_and_host_are_rejected() {
        let (bridge, secret) = start().await;
        assert!(connect_with(&bridge, None, Some("https://evil.example"))
            .await
            .is_err());
        assert!(connect_with(&bridge, None, Some("http://localhost:3000"))
            .await
            .is_err());
        assert!(connect_with(&bridge, Some("evil.example"), None)
            .await
            .is_err());
        assert!(
            connect_with(&bridge, Some("127.0.0.1.evil.example:80"), None)
                .await
                .is_err()
        );

        // The renderer origin and loopback host names are fine.
        let mut ws = connect_with(
            &bridge,
            Some("localhost:1234"),
            Some("https://xpui.app.spotify.com"),
        )
        .await
        .unwrap();
        let reply = call(&mut ws, 1, "auth", serde_json::json!({ "secret": secret })).await;
        assert!(reply["result"].is_object());
        bridge.shutdown().await;
    }

    #[test]
    fn host_header_parsing() {
        for ok in [
            "127.0.0.1",
            "127.0.0.1:49421",
            "localhost",
            "localhost:80",
            "[::1]:80",
            "[::1]",
        ] {
            assert!(host_is_loopback(ok), "{ok}");
        }
        for bad in [
            "",
            "example.com",
            "127.0.0.1.evil.com",
            "localhost.evil.com:80",
            "[::2]:1",
        ] {
            assert!(!host_is_loopback(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn oversized_message_closes_the_connection() {
        let (bridge, secret) = start_with(
            |c| c.max_message_bytes = 2048,
            Router::with_builtin(Db::open_in_memory().unwrap()),
        )
        .await;
        let mut ws = authed(&bridge, &secret).await;
        let big = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "ping", "params": "x".repeat(8192)});
        let _ = ws.send(Message::text(big.to_string())).await;
        assert!(is_closed(&mut ws).await);
        bridge.shutdown().await;
    }

    #[tokio::test]
    async fn malformed_json_and_bad_envelopes_get_errors() {
        let (bridge, secret) = start().await;
        let mut ws = authed(&bridge, &secret).await;
        ws.send(Message::text("{not json")).await.unwrap();
        assert_eq!(recv(&mut ws).await["error"]["code"], PARSE_ERROR);
        ws.send(Message::text("[1,2]")).await.unwrap();
        assert_eq!(recv(&mut ws).await["error"]["code"], INVALID_REQUEST);
        ws.send(Message::text(r#"{"jsonrpc":"1.0","id":3,"method":"ping"}"#))
            .await
            .unwrap();
        let reply = recv(&mut ws).await;
        assert_eq!(reply["error"]["code"], INVALID_REQUEST);
        assert_eq!(reply["id"], 3);
        // Still usable afterwards.
        assert_eq!(
            call(&mut ws, 4, "ping", Value::Null).await["result"],
            "pong"
        );
        bridge.shutdown().await;
    }

    #[tokio::test]
    async fn requests_over_the_rate_limit_are_refused() {
        let (bridge, secret) = start_with(
            |c| c.rate_limit_per_sec = 5,
            Router::with_builtin(Db::open_in_memory().unwrap()),
        )
        .await;
        let mut ws = authed(&bridge, &secret).await;
        for id in 1..=30u64 {
            let request = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "ping"});
            ws.send(Message::text(request.to_string())).await.unwrap();
        }
        let mut limited = 0;
        let mut ok = 0;
        for _ in 0..30 {
            let reply = recv(&mut ws).await;
            if reply["error"]["code"] == RATE_LIMITED {
                limited += 1;
            } else if reply["result"] == "pong" {
                ok += 1;
            }
        }
        assert!(limited >= 1 && ok >= 1, "limited {limited}, ok {ok}");
        assert_eq!(limited + ok, 30);
        bridge.shutdown().await;
    }

    #[tokio::test]
    async fn notifications_reach_only_authenticated_clients() {
        let (bridge, secret) = start().await;
        let mut ws = authed(&bridge, &secret).await;
        let mut stranger = connect_with(&bridge, None, None).await.unwrap();
        assert_eq!(
            bridge.notify("theme.css", &serde_json::json!({"css": "a{}"})),
            1
        );
        let push = recv(&mut ws).await;
        assert_eq!(push["method"], "theme.css");
        assert_eq!(push["params"]["css"], "a{}");
        assert!(push.get("id").is_none());
        // The unauthenticated connection is never given the push; it just times out.
        assert!(timeout(Duration::from_millis(300), stranger.next())
            .await
            .is_err());
        bridge.shutdown().await;
    }

    #[tokio::test]
    async fn slow_handler_does_not_block_other_requests() {
        let mut router = Router::with_builtin(Db::open_in_memory().unwrap());
        router.register("slow", |_, _| async {
            tokio::time::sleep(Duration::from_millis(800)).await;
            Ok(serde_json::json!("slow done"))
        });
        let (bridge, secret) = start_with(|_| {}, router).await;
        let mut ws = authed(&bridge, &secret).await;
        let started = Instant::now();
        ws.send(Message::text(r#"{"jsonrpc":"2.0","id":1,"method":"slow"}"#))
            .await
            .unwrap();
        ws.send(Message::text(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#))
            .await
            .unwrap();
        let first = recv(&mut ws).await;
        assert_eq!(first["id"], 2, "ping must answer before the slow call");
        assert!(started.elapsed() < Duration::from_millis(600));
        let second = recv(&mut ws).await;
        assert_eq!(second["result"], "slow done");
        bridge.shutdown().await;
    }

    #[tokio::test]
    async fn requests_without_id_get_no_response() {
        let (bridge, secret) = start().await;
        let mut ws = authed(&bridge, &secret).await;
        ws.send(Message::text(r#"{"jsonrpc":"2.0","method":"ping"}"#))
            .await
            .unwrap();
        assert!(timeout(Duration::from_millis(300), ws.next())
            .await
            .is_err());
        assert_eq!(call(&mut ws, 9, "ping", Value::Null).await["id"], 9);
        bridge.shutdown().await;
    }

    #[tokio::test]
    async fn falls_back_to_the_next_free_port() {
        let blocker = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let busy = blocker.local_addr().unwrap().port();
        let free = {
            let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            probe.local_addr().unwrap().port()
        };
        let (bridge, _) = start_with(|c| c.ports = vec![busy, free], Router::new()).await;
        assert_eq!(bridge.port(), free);
        bridge.shutdown().await;

        let err = Bridge::start(
            BridgeConfig {
                ports: vec![busy],
                ..BridgeConfig::new(secret::generate().unwrap())
            },
            Router::new(),
        )
        .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn shutdown_closes_connected_clients() {
        let (bridge, secret) = start().await;
        let mut ws = authed(&bridge, &secret).await;
        let port = bridge.port();
        bridge.shutdown().await;
        assert!(is_closed(&mut ws).await);
        assert!(tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn invalid_configuration_is_rejected() {
        for edit in [
            (|c: &mut BridgeConfig| c.secret = "short".into()) as fn(&mut BridgeConfig),
            |c| c.ports.clear(),
            |c| c.max_message_bytes = 10,
            |c| c.rate_limit_per_sec = 0,
            |c| c.max_in_flight = 0,
        ] {
            let mut config = BridgeConfig::new(secret::generate().unwrap());
            edit(&mut config);
            assert!(Bridge::start(config, Router::new()).await.is_err());
        }
    }

    #[test]
    fn rate_limiter_resets_each_window() {
        let mut limiter = RateLimiter::new(2);
        assert!(limiter.allow() && limiter.allow() && !limiter.allow());
        limiter.window_start = Instant::now() - Duration::from_secs(2);
        assert!(limiter.allow());
    }
}
