//! Logging setup based on `tracing`.

use crate::error::{Error, Result};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Installs the global subscriber. The filter is taken from `RUST_LOG` and
/// falls back to `default_filter` (for example `"info"`). Calling this twice
/// is an error because the subscriber can only be installed once.
pub fn init(default_filter: &str) -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_filter))
        .map_err(|e| Error::Config(format!("invalid log filter `{default_filter}`: {e}")))?;
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(true).compact())
        .try_init()
        .map_err(|e| Error::Internal(format!("logging already initialised: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_filter_is_rejected() {
        // `RUST_LOG` may be set in the environment of the test runner, so only
        // the failure path with an explicit broken directive is asserted.
        if std::env::var_os("RUST_LOG").is_none() {
            assert_eq!(init("[[[").unwrap_err().code(), "config");
        }
    }
}
