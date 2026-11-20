//! Detection of Spotify Desktop installations and their versions.

pub mod detect;
pub mod version;

pub use detect::{
    candidates, detect, detect_current, probe, probe_custom, Candidate, DetectedInstall, Env,
    Platform, SPA_FILE,
};
pub use version::{SpotifyVersion, VersionRange};
