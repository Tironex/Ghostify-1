//! Shared foundation of Ghostify: errors, configuration, paths, events and logging.

pub mod config;
pub mod error;
pub mod events;
pub mod logging;
pub mod paths;

pub use config::Config;
pub use error::{ApiError, Error, Result};
pub use events::{Event, EventBus, PluginState};
pub use paths::AppPaths;
