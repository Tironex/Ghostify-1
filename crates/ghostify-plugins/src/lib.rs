//! Plugin system: manifests, permissions, scanner, versioned store, bridge methods.

pub mod manifest;
pub mod package;
pub mod rpc;
pub mod scanner;
pub mod store;

pub use manifest::{Permission, PluginManifest};
pub use scanner::{Finding, ScanReport, Severity};
pub use store::{
    CrashOutcome, EnabledPlugin, Inspection, InstallOptions, Installed, LoadedPlugin, PluginInfo,
    PluginSource, PluginStore,
};
