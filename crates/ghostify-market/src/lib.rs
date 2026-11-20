//! The marketplace without a server of our own: a signed index file hosted on
//! GitHub, verified downloads from GitHub, search and state on the user's machine.

pub mod fetch;
pub mod index;
pub mod service;
pub mod signed;

pub use fetch::{Fetcher, UreqFetcher};
pub use index::{Index, IndexEntry};
pub use service::{Marketplace, MarketplaceDeps, RefreshOutcome, UpdateInfo};
pub use signed::{SignedIndex, TrustedKey};
