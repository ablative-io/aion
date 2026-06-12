//! Environment variable overlays for `AION_` prefixed server configuration.

use std::net::SocketAddr;

use crate::{
    config::{ServerConfig, StoreBackend, config_error},
    error::ServerError,
};

/// Apply supported `AION_` environment variable overrides to a config value.
///
/// # Errors
///
/// Returns [`ServerError::Config`] when an environment variable cannot be parsed into the target
/// typed field.
pub fn overlay(config: &mut ServerConfig) -> Result<(), ServerError> {
    for (name, value) in std::env::vars() {
        match name.as_str() {
            "AION_SERVER_LISTEN_ADDRESS" => {
                config.server.listen_address = parse_socket_addr(&name, &value)?;
            }
            "AION_SERVER_GRPC_ADDRESS" => {
                config.server.grpc_address = parse_socket_addr(&name, &value)?;
            }
            "AION_STORE_BACKEND" => {
                config.store.backend = parse_store_backend(&name, &value)?;
            }
            "AION_STORE_URL" => {
                if value.is_empty() {
                    return config_error("AION_STORE_URL must not be empty");
                }
                config.store.url = Some(value);
                if config.store.backend == StoreBackend::Memory {
                    config.store.backend = StoreBackend::LibSql;
                }
            }
            "AION_RUNTIME_SCHEDULER_THREADS" => {
                config.runtime.scheduler_threads = parse_positive_usize(&name, &value)?;
            }
            "AION_RUNTIME_QUERY_TIMEOUT_MS" => {
                config.runtime.query_timeout_ms = Some(parse_positive_u64(&name, &value)?);
            }
            "AION_DRAIN_TIMEOUT_SECONDS" => {
                config.drain.timeout_seconds = parse_positive_u64(&name, &value)?;
            }
            "AION_AUTH_ENABLED" => {
                config.auth.enabled = parse_bool(&name, &value)?;
            }
            "AION_AUTH_JWKS_URL" => {
                if value.is_empty() {
                    return config_error("AION_AUTH_JWKS_URL must not be empty");
                }
                config.auth.jwks_url = Some(value);
            }
            "AION_AUTH_JWKS_REFRESH_SECONDS" => {
                config.auth.jwks_refresh_seconds = parse_positive_u64(&name, &value)?;
            }
            "AION_METRICS_ENABLED" => {
                config.metrics.enabled = parse_bool(&name, &value)?;
            }
            "AION_WEBSOCKET_OUTBOUND_BUFFER_BOUND" => {
                config.websocket.outbound_buffer_bound = parse_positive_usize(&name, &value)?;
            }
            "AION_WEBSOCKET_EVENT_BROADCAST_CAPACITY" => {
                config.websocket.event_broadcast_capacity =
                    Some(parse_positive_usize(&name, &value)?);
            }
            "AION_DEPLOY_ENABLED" => {
                config.deploy.enabled = parse_bool(&name, &value)?;
            }
            "AION_DEPLOY_MAX_ARCHIVE_BYTES" => {
                config.deploy.max_archive_bytes = Some(parse_positive_u64(&name, &value)?);
            }
            "AION_NAMESPACES_DEFAULT" => {
                if value.is_empty() {
                    return config_error("AION_NAMESPACES_DEFAULT must not be empty");
                }
                config.namespaces.default = value;
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_socket_addr(name: &str, value: &str) -> Result<SocketAddr, ServerError> {
    value.parse().map_err(|source| ServerError::Config {
        message: format!("{name} must be a socket address: {source}"),
    })
}

fn parse_store_backend(name: &str, value: &str) -> Result<StoreBackend, ServerError> {
    match value.to_ascii_lowercase().as_str() {
        "memory" => Ok(StoreBackend::Memory),
        "libsql" => Ok(StoreBackend::LibSql),
        _ => config_error(format!("{name} must be one of: memory, libsql")),
    }
}

fn parse_positive_usize(name: &str, value: &str) -> Result<usize, ServerError> {
    let parsed = value
        .parse::<usize>()
        .map_err(|source| ServerError::Config {
            message: format!("{name} must be a positive integer: {source}"),
        })?;
    if parsed == 0 {
        return config_error(format!("{name} must be a positive integer"));
    }
    Ok(parsed)
}

fn parse_positive_u64(name: &str, value: &str) -> Result<u64, ServerError> {
    let parsed = value.parse::<u64>().map_err(|source| ServerError::Config {
        message: format!("{name} must be a positive integer: {source}"),
    })?;
    if parsed == 0 {
        return config_error(format!("{name} must be a positive integer"));
    }
    Ok(parsed)
}

fn parse_bool(name: &str, value: &str) -> Result<bool, ServerError> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => config_error(format!("{name} must be a boolean")),
    }
}
