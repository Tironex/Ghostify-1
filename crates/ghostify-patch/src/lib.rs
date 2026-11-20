//! Patch engine of Ghostify: signed rules, verified backups, atomic patching,
//! recovery and automatic repatch after Spotify updates.

pub mod auto;
pub mod backup;
pub mod engine;
pub mod fsutil;
pub mod manager;
pub mod rules;
pub mod signing;
pub mod source;
pub mod watch;

pub use auto::AutoRepatch;
pub use backup::{BackupEntry, BackupManifest, BackupStore};
pub use engine::{apply, read_patch_info, verify, ApplyReport, Asset, PatchInfo};
pub use manager::{DiskState, PatchManager, PatchReport, PatchResult, StatusReport};
pub use rules::{Action, Rule, RuleSet};
pub use signing::{SignedRuleSet, TrustedKey};
pub use source::{DirSource, LoaderConfig, PatchSource, StaticSource};
pub use watch::AppsWatcher;
