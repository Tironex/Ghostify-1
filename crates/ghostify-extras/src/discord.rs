//! Discord Rich Presence over Discord's local IPC (a Unix socket or a Windows
//! named pipe). Nothing leaves the computer except through the Discord client.

use crate::player::{PlayerSnapshot, Track};
use ghostify_core::{Error, Result};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const MAX_FRAME: u32 = 64 * 1024;

pub trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

fn write_frame(stream: &mut dyn Stream, opcode: u32, payload: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(payload)?;
    let mut frame = Vec::with_capacity(8 + body.len());
    frame.extend_from_slice(&opcode.to_le_bytes());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    stream.write_all(&frame)?;
    stream.flush()
}

fn read_frame(stream: &mut dyn Stream) -> std::io::Result<(u32, Value)> {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header)?;
    let opcode = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    if length > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Discord sent an oversized frame",
        ));
    }
    let mut body = vec![0u8; length as usize];
    stream.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok((opcode, value))
}

/// One connection to the Discord client.
pub struct DiscordClient {
    stream: Box<dyn Stream>,
    pid: u32,
    nonce: u64,
}

fn io_error(context: &str, error: std::io::Error) -> Error {
    Error::Io(std::io::Error::new(
        error.kind(),
        format!("{context}: {error}"),
    ))
}

impl DiscordClient {
    /// Performs the handshake. Fails if Discord rejects the application id.
    pub fn handshake(
        mut stream: Box<dyn Stream>,
        client_id: &str,
        pid: u32,
    ) -> Result<DiscordClient> {
        write_frame(
            &mut *stream,
            OP_HANDSHAKE,
            &json!({ "v": 1, "client_id": client_id }),
        )
        .map_err(|e| io_error("Discord handshake", e))?;
        let (opcode, reply) =
            read_frame(&mut *stream).map_err(|e| io_error("Discord handshake", e))?;
        if opcode == OP_CLOSE || reply["evt"] == "ERROR" {
            let message = reply["message"]
                .as_str()
                .unwrap_or("Discord refused the connection");
            return Err(Error::Invalid(format!("Discord: {message}")));
        }
        if reply["evt"] != "READY" {
            return Err(Error::Invalid(
                "Discord sent an unexpected answer to the handshake".into(),
            ));
        }
        Ok(DiscordClient {
            stream,
            pid,
            nonce: 0,
        })
    }

    /// Shows `activity`, or clears the presence for `None`.
    pub fn set_activity(&mut self, activity: Option<&Value>) -> Result<()> {
        self.nonce += 1;
        let request = json!({
            "cmd": "SET_ACTIVITY",
            "args": { "pid": self.pid, "activity": activity },
            "nonce": self.nonce.to_string(),
        });
        write_frame(&mut *self.stream, OP_FRAME, &request).map_err(|e| io_error("Discord", e))?;
        let (_, reply) = read_frame(&mut *self.stream).map_err(|e| io_error("Discord", e))?;
        if reply["evt"] == "ERROR" {
            let message = reply["data"]["message"].as_str().unwrap_or("unknown error");
            return Err(Error::Invalid(format!(
                "Discord rejected the presence: {message}"
            )));
        }
        Ok(())
    }
}

fn limit(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Discord wants 2 to 128 characters for the text lines.
fn line(text: &str, fallback: &str) -> String {
    let text = limit(text.trim(), 128);
    if text.chars().count() >= 2 {
        text
    } else {
        fallback.to_owned()
    }
}

/// The activity for what is playing, or `None` if the presence should be cleared
/// (nothing playing or paused).
pub fn activity_for(snapshot: &PlayerSnapshot, now_unix_secs: u64) -> Option<Value> {
    let track: &Track = snapshot.track.as_ref()?;
    if !snapshot.playing {
        return None;
    }
    let start = now_unix_secs.saturating_sub(snapshot.position_ms / 1000);
    let mut timestamps = json!({ "start": start });
    if let Some(duration) = track.duration_ms {
        timestamps["end"] = json!(start + duration / 1000);
    }
    let mut activity = json!({
        "type": 2,
        "details": line(&track.title, "Unknown title"),
        "state": line(&format!("by {}", track.artist), "Unknown artist"),
        "timestamps": timestamps,
    });
    if let Some(artwork) = &track.artwork {
        activity["assets"] =
            json!({ "large_image": artwork, "large_text": line(&track.album, "Album") });
    }
    Some(activity)
}

/// Opens the first Discord IPC endpoint that answers.
#[cfg(unix)]
pub fn connect_ipc() -> std::io::Result<Box<dyn Stream>> {
    use std::os::unix::net::UnixStream;
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for var in ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"] {
        if let Some(dir) = std::env::var_os(var) {
            let dir = std::path::PathBuf::from(dir);
            // Flatpak and Snap keep the socket in a sub folder.
            dirs.push(dir.join("app/com.discordapp.Discord"));
            dirs.push(dir.join("snap.discord"));
            dirs.push(dir);
        }
    }
    dirs.push("/tmp".into());
    for dir in dirs {
        for index in 0..10 {
            if let Ok(stream) = UnixStream::connect(dir.join(format!("discord-ipc-{index}"))) {
                stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                return Ok(Box::new(stream));
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Discord is not running",
    ))
}

#[cfg(windows)]
pub fn connect_ipc() -> std::io::Result<Box<dyn Stream>> {
    for index in 0..10 {
        let path = format!(r"\\.\pipe\discord-ipc-{index}");
        if let Ok(pipe) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
        {
            return Ok(Box::new(pipe));
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Discord is not running",
    ))
}

/// Keeps the presence in sync with the snapshots it receives until the sender is dropped.
/// Runs on its own thread: the IPC calls block.
pub fn run_presence(
    rx: Receiver<PlayerSnapshot>,
    client_id: String,
    connect: impl Fn() -> std::io::Result<Box<dyn Stream>>,
    now_unix_secs: impl Fn() -> u64,
    retry_delay: Duration,
) {
    let pid = std::process::id();
    let mut client: Option<DiscordClient> = None;
    let mut wanted: Option<Value> = None;
    let mut applied: Option<Option<Value>> = None;
    loop {
        match rx.recv_timeout(retry_delay) {
            Ok(mut snapshot) => {
                // Only the newest state matters.
                while let Ok(newer) = rx.try_recv() {
                    snapshot = newer;
                }
                wanted = activity_for(&snapshot, now_unix_secs());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if applied.as_ref() == Some(&wanted) && client.is_some() {
            continue;
        }
        if client.is_none() {
            match connect()
                .map_err(Error::from)
                .and_then(|s| DiscordClient::handshake(s, &client_id, pid))
            {
                Ok(connected) => client = Some(connected),
                Err(err) => {
                    tracing::debug!(%err, "Discord is not available");
                    applied = None;
                    continue;
                }
            }
        }
        if let Some(active) = client.as_mut() {
            match active.set_activity(wanted.as_ref()) {
                Ok(()) => applied = Some(wanted.clone()),
                Err(err) => {
                    tracing::debug!(%err, "Discord presence failed; reconnecting");
                    client = None;
                    applied = None;
                }
            }
        }
    }
    if let Some(mut active) = client {
        let _ = active.set_activity(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(playing: bool, position_ms: u64) -> PlayerSnapshot {
        PlayerSnapshot {
            playing,
            position_ms,
            track: Some(Track {
                title: "Song".into(),
                artist: "Artist".into(),
                album: "Album".into(),
                artwork: Some("https://i.scdn.co/image/x".into()),
                duration_ms: Some(180_000),
            }),
        }
    }

    #[test]
    fn the_activity_describes_the_track_with_timestamps() {
        let activity = activity_for(&snapshot(true, 30_000), 1_000_000).unwrap();
        assert_eq!(activity["details"], "Song");
        assert_eq!(activity["state"], "by Artist");
        assert_eq!(
            activity["timestamps"],
            json!({"start": 999_970, "end": 1_000_150})
        );
        assert_eq!(
            activity["assets"]["large_image"],
            "https://i.scdn.co/image/x"
        );
        assert_eq!(activity["type"], 2);
    }

    #[test]
    fn paused_or_empty_clears_the_presence() {
        assert!(activity_for(&snapshot(false, 0), 1).is_none());
        assert!(activity_for(
            &PlayerSnapshot {
                playing: false,
                track: None,
                position_ms: 0
            },
            1
        )
        .is_none());
    }

    #[test]
    fn text_lines_respect_discords_limits() {
        let mut s = snapshot(true, 0);
        let track = s.track.as_mut().unwrap();
        track.title = "x".into();
        track.artist = String::new();
        track.artwork = None;
        let activity = activity_for(&s, 100).unwrap();
        assert_eq!(activity["details"], "Unknown title");
        assert_eq!(activity["state"], "by");
        assert!(activity.get("assets").is_none());
        s.track.as_mut().unwrap().title = "ä".repeat(300);
        assert_eq!(
            activity_for(&s, 100).unwrap()["details"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            128
        );
    }

    #[test]
    fn the_start_never_underflows() {
        let activity = activity_for(&snapshot(true, 500_000), 10).unwrap();
        assert_eq!(activity["timestamps"]["start"], 0);
    }

    #[cfg(unix)]
    mod ipc {
        use super::*;
        use std::os::unix::net::UnixStream;
        use std::sync::{mpsc, Arc, Mutex};

        /// A Discord stand-in: answers the handshake and records presence requests.
        fn fake_discord(mut stream: UnixStream, log: Arc<Mutex<Vec<Value>>>, reject_id: bool) {
            std::thread::spawn(move || {
                let Ok((_, hello)) = read_frame(&mut stream) else {
                    return;
                };
                if reject_id {
                    let _ = write_frame(
                        &mut stream,
                        OP_CLOSE,
                        &json!({"code": 4000, "message": "Invalid Client ID"}),
                    );
                    return;
                }
                log.lock().unwrap().push(hello);
                write_frame(
                    &mut stream,
                    OP_FRAME,
                    &json!({"cmd": "DISPATCH", "evt": "READY", "data": {}}),
                )
                .unwrap();
                while let Ok((_, request)) = read_frame(&mut stream) {
                    let nonce = request["nonce"].clone();
                    log.lock().unwrap().push(request);
                    if write_frame(
                        &mut stream,
                        OP_FRAME,
                        &json!({"cmd": "SET_ACTIVITY", "evt": null, "nonce": nonce}),
                    )
                    .is_err()
                    {
                        return;
                    }
                }
            });
        }

        #[test]
        fn handshake_and_presence_follow_the_protocol() {
            let (ours, theirs) = UnixStream::pair().unwrap();
            let log = Arc::new(Mutex::new(Vec::new()));
            fake_discord(theirs, log.clone(), false);
            let mut client =
                DiscordClient::handshake(Box::new(ours), "123456789012345678", 42).unwrap();
            client
                .set_activity(Some(&json!({"details": "Song"})))
                .unwrap();
            client.set_activity(None).unwrap();
            let log = log.lock().unwrap();
            assert_eq!(log[0], json!({"v": 1, "client_id": "123456789012345678"}));
            assert_eq!(log[1]["cmd"], "SET_ACTIVITY");
            assert_eq!(
                log[1]["args"],
                json!({"pid": 42, "activity": {"details": "Song"}})
            );
            assert_eq!(log[2]["args"]["activity"], Value::Null);
            assert_ne!(log[1]["nonce"], log[2]["nonce"]);
        }

        #[test]
        fn a_rejected_client_id_is_reported() {
            let (ours, theirs) = UnixStream::pair().unwrap();
            fake_discord(theirs, Arc::default(), true);
            let err = DiscordClient::handshake(Box::new(ours), "1", 1)
                .err()
                .unwrap();
            assert!(err.to_string().contains("Invalid Client ID"), "{err}");
        }

        #[test]
        fn oversized_frames_are_refused() {
            let (ours, mut theirs) = UnixStream::pair().unwrap();
            theirs.write_all(&[1, 0, 0, 0]).unwrap();
            theirs.write_all(&(MAX_FRAME + 1).to_le_bytes()).unwrap();
            let mut stream: Box<dyn Stream> = Box::new(ours);
            assert!(read_frame(&mut *stream).is_err());
        }

        #[test]
        fn the_worker_connects_later_follows_the_player_and_clears_on_exit() {
            let log = Arc::new(Mutex::new(Vec::new()));
            let attempts = Arc::new(Mutex::new(0u32));
            let (tx, rx) = mpsc::channel();
            let (worker_log, worker_attempts) = (log.clone(), attempts.clone());
            let handle = std::thread::spawn(move || {
                run_presence(
                    rx,
                    "123456789012345678".into(),
                    move || {
                        let mut attempts = worker_attempts.lock().unwrap();
                        *attempts += 1;
                        if *attempts < 3 {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::NotFound,
                                "Discord is not running",
                            ));
                        }
                        let (ours, theirs) = UnixStream::pair()?;
                        fake_discord(theirs, worker_log.clone(), false);
                        Ok(Box::new(ours) as Box<dyn Stream>)
                    },
                    || 1_000_000,
                    Duration::from_millis(20),
                );
            });
            tx.send(snapshot(true, 0)).unwrap();
            let wait = |what: &str, check: &dyn Fn(&[Value]) -> bool| {
                for _ in 0..200 {
                    if check(&log.lock().unwrap()) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                panic!("timed out waiting for {what}: {:?}", log.lock().unwrap());
            };
            wait("the first presence after Discord appeared", &|l| {
                l.iter().any(|r| r["args"]["activity"]["details"] == "Song")
            });
            assert!(*attempts.lock().unwrap() >= 3);
            tx.send(snapshot(false, 5_000)).unwrap();
            wait("the presence to be cleared on pause", &|l| {
                l.last()
                    .is_some_and(|r| r["cmd"] == "SET_ACTIVITY" && r["args"]["activity"].is_null())
            });
            tx.send(snapshot(true, 5_000)).unwrap();
            wait("playing again", &|l| {
                l.last()
                    .is_some_and(|r| r["args"]["activity"]["details"] == "Song")
            });
            let sent = log.lock().unwrap().len();
            // The same state again must not be sent twice.
            tx.send(snapshot(true, 5_000)).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            assert_eq!(log.lock().unwrap().len(), sent);
            drop(tx);
            handle.join().unwrap();
            assert!(
                log.lock().unwrap().last().unwrap()["args"]["activity"].is_null(),
                "cleared when the worker stops"
            );
        }
    }
}
