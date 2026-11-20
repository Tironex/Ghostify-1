//! What is playing right now, as reported by the loader inside Spotify, and the
//! commands sent back to it.

use ghostify_bridge::{Router, RpcError};
use ghostify_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::broadcast;

const MAX_TEXT: usize = 300;
const MAX_URL: usize = 512;
/// A position that differs more than this from the extrapolated one is a seek.
const SEEK_TOLERANCE_MS: u64 = 2_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub artwork: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlayerSnapshot {
    pub playing: bool,
    pub track: Option<Track>,
    pub position_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerCommand {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
}

impl PlayerCommand {
    pub fn as_str(self) -> &'static str {
        match self {
            PlayerCommand::Play => "play",
            PlayerCommand::Pause => "pause",
            PlayerCommand::Toggle => "toggle",
            PlayerCommand::Next => "next",
            PlayerCommand::Previous => "previous",
        }
    }
}

/// Sends a notification to every connected loader.
pub type Notify = Arc<dyn Fn(&str, &Value) + Send + Sync>;

/// Path to the loaders: sends a notification and tells how many are connected.
#[derive(Clone)]
pub struct PlayerLink {
    pub notify: Notify,
    pub connected: Arc<dyn Fn() -> usize + Send + Sync>,
}

#[derive(Debug, Deserialize)]
struct Report {
    playing: bool,
    #[serde(default)]
    title: String,
    #[serde(default)]
    artist: String,
    #[serde(default)]
    album: String,
    #[serde(default)]
    artwork: Option<String>,
    #[serde(default)]
    position_ms: Option<u64>,
    #[serde(default)]
    duration_ms: Option<u64>,
}

struct State {
    playing: bool,
    track: Option<Track>,
    position_ms: u64,
    at: Instant,
}

struct Inner {
    state: Mutex<State>,
    tx: broadcast::Sender<PlayerSnapshot>,
    link: Mutex<Option<PlayerLink>>,
}

#[derive(Clone)]
pub struct PlayerHub {
    inner: Arc<Inner>,
}

impl Default for PlayerHub {
    fn default() -> Self {
        PlayerHub::new()
    }
}

fn clean(label: &str, value: &str) -> Result<String> {
    if value.chars().count() > MAX_TEXT || value.chars().any(char::is_control) {
        return Err(Error::Invalid(format!(
            "{label} must be at most {MAX_TEXT} characters without control characters"
        )));
    }
    Ok(value.trim().to_owned())
}

impl PlayerHub {
    pub fn new() -> PlayerHub {
        let (tx, _) = broadcast::channel(16);
        PlayerHub {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    playing: false,
                    track: None,
                    position_ms: 0,
                    at: Instant::now(),
                }),
                tx,
                link: Mutex::new(None),
            }),
        }
    }

    pub fn set_link(&self, link: PlayerLink) {
        *self.inner.link.lock().unwrap_or_else(|p| p.into_inner()) = Some(link);
    }

    /// Changes are broadcast; position-only updates are not, unless the position jumped.
    pub fn subscribe(&self) -> broadcast::Receiver<PlayerSnapshot> {
        self.inner.tx.subscribe()
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        self.snapshot_at(Instant::now())
    }

    fn snapshot_at(&self, now: Instant) -> PlayerSnapshot {
        let state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        snapshot_of(&state, now)
    }

    /// Applies a report from the loader.
    pub fn report(&self, params: Value) -> Result<()> {
        self.report_at(params, Instant::now())
    }

    fn report_at(&self, params: Value, now: Instant) -> Result<()> {
        let report: Report = serde_json::from_value(params)
            .map_err(|e| Error::Invalid(format!("invalid player report: {e}")))?;
        let title = clean("title", &report.title)?;
        let artist = clean("artist", &report.artist)?;
        let album = clean("album", &report.album)?;
        let artwork = match report.artwork {
            Some(url) if url.is_empty() => None,
            Some(url) => {
                if !url.starts_with("https://")
                    || url.len() > MAX_URL
                    || url.chars().any(|c| c.is_control() || c == ' ')
                {
                    return Err(Error::Invalid("artwork must be an https address".into()));
                }
                Some(url)
            }
            None => None,
        };
        let track = (!title.is_empty()).then_some(Track {
            title,
            artist,
            album,
            artwork,
            duration_ms: report.duration_ms,
        });
        let duration = track.as_ref().and_then(|t| t.duration_ms);

        let snapshot = {
            let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
            let before = snapshot_of(&state, now);
            let mut position = report.position_ms.unwrap_or(before.position_ms);
            if let Some(duration) = duration {
                position = position.min(duration);
            }
            // Without a new position the old one continues only for the same track.
            if report.position_ms.is_none() && before.track != track {
                position = 0;
            }
            let after = PlayerSnapshot {
                playing: report.playing && track.is_some(),
                track,
                position_ms: position,
            };
            let jumped = before.position_ms.abs_diff(after.position_ms) > SEEK_TOLERANCE_MS;
            let changed = before.playing != after.playing || before.track != after.track || jumped;
            *state = State {
                playing: after.playing,
                track: after.track.clone(),
                position_ms: after.position_ms,
                at: now,
            };
            changed.then_some(after)
        };
        if let Some(snapshot) = snapshot {
            // No receiver is fine.
            let _ = self.inner.tx.send(snapshot);
        }
        Ok(())
    }

    /// Asks the loader to press a player button.
    pub fn command(&self, command: PlayerCommand) -> Result<()> {
        let link = self
            .inner
            .link
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let link = link.ok_or_else(|| Error::NotFound("Spotify is not connected".into()))?;
        if (link.connected)() == 0 {
            return Err(Error::NotFound("Spotify is not connected".into()));
        }
        (link.notify)("player.command", &json!({ "action": command.as_str() }));
        Ok(())
    }

    /// `player.report`: called by the loader whenever the track or play state changes.
    pub fn register(&self, router: &mut Router) {
        let hub = self.clone();
        router.register("player.report", move |_, params| {
            let hub = hub.clone();
            async move {
                hub.report(params).map_err(RpcError::from)?;
                Ok(Value::Null)
            }
        });
    }
}

fn snapshot_of(state: &State, now: Instant) -> PlayerSnapshot {
    let mut position = state.position_ms;
    if state.playing {
        position += now.saturating_duration_since(state.at).as_millis() as u64;
    }
    if let Some(duration) = state.track.as_ref().and_then(|t| t.duration_ms) {
        position = position.min(duration);
    }
    PlayerSnapshot {
        playing: state.playing,
        track: state.track.clone(),
        position_ms: position,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn report(title: &str, playing: bool, position: Option<u64>) -> Value {
        json!({"playing": playing, "title": title, "artist": "Artist", "album": "Album", "position_ms": position, "duration_ms": 200_000})
    }

    #[test]
    fn position_advances_only_while_playing() {
        let hub = PlayerHub::new();
        let t0 = Instant::now();
        hub.report_at(report("Song", true, Some(10_000)), t0)
            .unwrap();
        assert_eq!(
            hub.snapshot_at(t0 + Duration::from_secs(5)).position_ms,
            15_000
        );
        hub.report_at(
            report("Song", false, Some(15_000)),
            t0 + Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            hub.snapshot_at(t0 + Duration::from_secs(60)).position_ms,
            15_000
        );
        // Never beyond the end of the track.
        hub.report_at(report("Song", true, Some(199_000)), t0)
            .unwrap();
        assert_eq!(
            hub.snapshot_at(t0 + Duration::from_secs(30)).position_ms,
            200_000
        );
    }

    #[test]
    fn only_real_changes_are_broadcast() {
        let hub = PlayerHub::new();
        let mut rx = hub.subscribe();
        let t0 = Instant::now();
        hub.report_at(report("A", true, Some(0)), t0).unwrap();
        assert_eq!(rx.try_recv().unwrap().track.unwrap().title, "A");
        // Regular resync with the expected position: silent.
        hub.report_at(
            report("A", true, Some(10_000)),
            t0 + Duration::from_secs(10),
        )
        .unwrap();
        assert!(rx.try_recv().is_err());
        // A seek is a change.
        hub.report_at(
            report("A", true, Some(100_000)),
            t0 + Duration::from_secs(11),
        )
        .unwrap();
        assert_eq!(rx.try_recv().unwrap().position_ms, 100_000);
        // Pause, then another track.
        hub.report_at(
            report("A", false, Some(100_500)),
            t0 + Duration::from_secs(12),
        )
        .unwrap();
        assert!(!rx.try_recv().unwrap().playing);
        hub.report_at(report("B", true, None), t0 + Duration::from_secs(13))
            .unwrap();
        let next = rx.try_recv().unwrap();
        assert_eq!(
            (next.track.unwrap().title.as_str(), next.position_ms),
            ("B", 0)
        );
    }

    #[test]
    fn an_empty_title_means_nothing_is_playing() {
        let hub = PlayerHub::new();
        hub.report(report("A", true, Some(0))).unwrap();
        hub.report(report("", true, None)).unwrap();
        let snapshot = hub.snapshot();
        assert!(snapshot.track.is_none() && !snapshot.playing);
    }

    #[test]
    fn invalid_reports_are_rejected_and_change_nothing() {
        let hub = PlayerHub::new();
        hub.report(report("A", true, Some(0))).unwrap();
        for bad in [
            json!({"title": "x"}),
            json!({"playing": "yes"}),
            json!({"playing": true, "title": "x".repeat(301)}),
            json!({"playing": true, "title": "a\u{7}b"}),
            json!({"playing": true, "title": "x", "artwork": "http://insecure/x.png"}),
            json!({"playing": true, "title": "x", "artwork": "javascript:alert(1)"}),
            json!({"playing": true, "title": "x", "artwork": format!("https://{}", "a".repeat(600))}),
            json!({"playing": true, "title": "x", "position_ms": -5}),
        ] {
            assert_eq!(
                hub.report(bad.clone()).unwrap_err().code(),
                "invalid_input",
                "{bad}"
            );
        }
        assert_eq!(hub.snapshot().track.unwrap().title, "A");
    }

    #[test]
    fn artwork_must_be_https() {
        let hub = PlayerHub::new();
        hub.report(
            json!({"playing": true, "title": "x", "artwork": "https://i.scdn.co/image/abc"}),
        )
        .unwrap();
        assert_eq!(
            hub.snapshot().track.unwrap().artwork.as_deref(),
            Some("https://i.scdn.co/image/abc")
        );
    }

    #[test]
    fn commands_need_a_connected_loader() {
        let hub = PlayerHub::new();
        assert_eq!(
            hub.command(PlayerCommand::Next).unwrap_err().code(),
            "not_found"
        );

        let sent: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let connected = Arc::new(Mutex::new(0usize));
        let (log, count) = (sent.clone(), connected.clone());
        hub.set_link(PlayerLink {
            notify: Arc::new(move |method, params| {
                log.lock()
                    .unwrap()
                    .push((method.to_owned(), params.clone()))
            }),
            connected: Arc::new(move || *count.lock().unwrap()),
        });
        assert_eq!(
            hub.command(PlayerCommand::Next).unwrap_err().code(),
            "not_found"
        );
        *connected.lock().unwrap() = 1;
        hub.command(PlayerCommand::Toggle).unwrap();
        assert_eq!(
            sent.lock().unwrap().as_slice(),
            [("player.command".to_owned(), json!({"action": "toggle"}))]
        );
    }

    #[tokio::test]
    async fn the_router_method_updates_the_hub() {
        let hub = PlayerHub::new();
        let mut router = Router::new();
        hub.register(&mut router);
        let ctx = ghostify_bridge::router::CallContext { session_id: 1 };
        router
            .dispatch(ctx, "player.report", report("Via bridge", true, Some(0)))
            .await
            .unwrap();
        assert_eq!(hub.snapshot().track.unwrap().title, "Via bridge");
        let err = router
            .dispatch(ctx, "player.report", json!({"nope": 1}))
            .await
            .unwrap_err();
        assert_eq!(err.code, ghostify_bridge::protocol::INVALID_PARAMS);
    }
}
