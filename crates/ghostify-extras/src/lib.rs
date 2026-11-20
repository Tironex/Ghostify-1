//! Features around the music that is playing: the player state reported by the
//! loader, lyrics, Discord Rich Presence and global hotkeys.

pub mod discord;
pub mod hotkeys;
pub mod lyrics;
pub mod player;

pub use hotkeys::{HotkeyAction, HotkeyService};
pub use lyrics::{Lyrics, LyricsService};
pub use player::{PlayerCommand, PlayerHub, PlayerLink, PlayerSnapshot, Track};
