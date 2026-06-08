//! Structured JSON tracing subscriber initialization.

use tracing_subscriber::EnvFilter;

use crate::ServerError;

/// Initialize the process-global tracing subscriber for production server output.
///
/// Events are written as JSON to stdout. `AION_LOG` takes precedence over
/// `RUST_LOG`, and the subscriber falls back to `info` when neither is set.
///
/// # Errors
///
/// Returns [`ServerError`] when the configured filter directive is invalid or
/// another subscriber has already been installed.
pub fn init() -> Result<(), ServerError> {
    let filter = env_filter()?;

    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_current_span(true)
        .with_span_list(true)
        .with_target(true)
        .with_timer(tracing_subscriber::fmt::time::SystemTime)
        .try_init()
        .map_err(|error| ServerError::Config {
            message: format!("failed to initialize tracing subscriber: {error}"),
        })
}

fn env_filter() -> Result<EnvFilter, ServerError> {
    let directive = std::env::var("AION_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "info".to_owned());

    EnvFilter::try_new(&directive).map_err(|error| ServerError::Config {
        message: format!("invalid log filter `{directive}`: {error}"),
    })
}
