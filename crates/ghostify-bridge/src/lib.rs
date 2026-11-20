//! Local JSON-RPC 2.0 bridge between the Ghostify app and the loader that runs
//! inside the patched Spotify client. Loopback only, authenticated with a
//! per-installation secret.

pub mod protocol;
pub mod router;
pub mod secret;
pub mod server;

pub use protocol::{Notification, Request, Response, RpcError};
pub use router::{CallContext, Router};
pub use server::{Bridge, BridgeConfig, DEFAULT_ALLOWED_ORIGINS};
