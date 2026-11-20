//! In-process event bus. The Tauri layer forwards these events to the UI and
//! the bridge forwards the relevant ones to plugins.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Lifecycle state of a plugin as reported to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
    Installed,
    Loaded,
    Active,
    Suspended,
    Crashed,
    Unloaded,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    PatchProgress {
        stage: String,
        percent: u8,
    },
    PluginState {
        plugin_id: String,
        state: PluginState,
    },
    PluginCrash {
        plugin_id: String,
        reason: String,
        rolled_back: bool,
    },
    MarketUpdateAvailable {
        listing_id: String,
        current_version: String,
        new_version: String,
    },
    PerfTick {
        cpu_percent: f32,
        memory_bytes: u64,
    },
    SpotifyVersionChanged {
        previous: Option<String>,
        current: String,
    },
}

impl Event {
    /// Name of the event on the Tauri IPC channel (`patch://progress`, ...).
    pub fn topic(&self) -> &'static str {
        match self {
            Event::PatchProgress { .. } => "patch://progress",
            Event::PluginState { .. } => "plugin://state",
            Event::PluginCrash { .. } => "plugin://crash",
            Event::MarketUpdateAvailable { .. } => "market://update-available",
            Event::PerfTick { .. } => "perf://tick",
            Event::SpotifyVersionChanged { .. } => "spotify://version-changed",
        }
    }
}

/// Multi-producer, multi-consumer broadcast channel for [`Event`]s.
/// Cloning the bus yields another handle to the same channel.
#[derive(Debug, Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    /// `capacity` is the number of events a slow subscriber may lag behind
    /// before it starts missing events.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(1));
        EventBus { tx }
    }

    /// Publishes an event and returns how many subscribers received it.
    /// Having no subscriber is normal and not an error.
    pub fn publish(&self, event: Event) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        EventBus::new(256)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribers_receive_published_events() {
        let bus = EventBus::new(8);
        let mut a = bus.subscribe();
        let mut b = bus.clone().subscribe();
        let event = Event::PluginState {
            plugin_id: "owner/repo".into(),
            state: PluginState::Active,
        };
        assert_eq!(bus.publish(event.clone()), 2);
        assert_eq!(a.recv().await.unwrap(), event);
        assert_eq!(b.recv().await.unwrap(), event);
    }

    #[test]
    fn publishing_without_subscribers_is_fine() {
        let bus = EventBus::default();
        assert_eq!(
            bus.publish(Event::PerfTick {
                cpu_percent: 0.2,
                memory_bytes: 1
            }),
            0
        );
    }

    #[test]
    fn topics_match_the_documented_names() {
        let event = Event::PatchProgress {
            stage: "backup".into(),
            percent: 10,
        };
        assert_eq!(event.topic(), "patch://progress");
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "patch_progress");
    }
}
