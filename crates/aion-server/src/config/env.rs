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
            "AION_SERVER_CORS_ALLOWED_ORIGINS" => {
                config.server.cors_allowed_origins = parse_csv_origins(&value);
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
            "AION_STORE_DATA_DIR" => {
                if value.is_empty() {
                    return config_error("AION_STORE_DATA_DIR must not be empty");
                }
                config.store.data_dir = Some(value);
            }
            "AION_STORE_SHARD_COUNT" => {
                config.store.shard_count = parse_positive_usize(&name, &value)?;
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
            "AION_DEPLOY_ENABLED" => {
                config.deploy.enabled = parse_bool(&name, &value)?;
            }
            "AION_DEPLOY_MAX_ARCHIVE_BYTES" => {
                config.deploy.max_archive_bytes = Some(parse_positive_u64(&name, &value)?);
            }
            "AION_DEPLOY_MAX_INFLATED_BYTES" => {
                config.deploy.max_inflated_bytes = Some(parse_positive_u64(&name, &value)?);
            }
            "AION_DEV_ENABLED" => {
                config.dev.enabled = parse_bool(&name, &value)?;
            }
            "AION_AUTHORING_GLEAM_PATH" => {
                if value.is_empty() {
                    return config_error("AION_AUTHORING_GLEAM_PATH must not be empty");
                }
                config.authoring.gleam_path = Some(std::path::PathBuf::from(value));
            }
            "AION_AUTHORING_PROJECT_ROOT" => {
                if value.is_empty() {
                    return config_error("AION_AUTHORING_PROJECT_ROOT must not be empty");
                }
                config.authoring.project_root = Some(std::path::PathBuf::from(value));
            }
            "AION_NAMESPACES_DEFAULT" => {
                if value.is_empty() {
                    return config_error("AION_NAMESPACES_DEFAULT must not be empty");
                }
                config.namespaces.default = value;
            }
            other => overlay_websocket(config, other, &value)?,
        }
    }
    Ok(())
}

/// Apply the WS3/streaming broadcast-capacity `AION_WEBSOCKET_*` overrides.
///
/// Split out of [`overlay`] so the broadcast-capacity knobs live together and
/// `overlay` stays within the per-function line budget. Unknown names fall
/// through to [`overlay_outbox`] and ultimately the silent-ignore default.
fn overlay_websocket(
    config: &mut ServerConfig,
    name: &str,
    value: &str,
) -> Result<(), ServerError> {
    match name {
        "AION_WEBSOCKET_EVENT_BROADCAST_CAPACITY" => {
            config.websocket.event_broadcast_capacity = Some(parse_positive_usize(name, value)?);
        }
        "AION_WEBSOCKET_CLUSTER_BROADCAST_CAPACITY" => {
            config.websocket.cluster_broadcast_capacity = Some(parse_positive_usize(name, value)?);
        }
        other => overlay_outbox(config, other, value)?,
    }
    Ok(())
}

/// Apply the `AION_OUTBOX_*` overrides for the durable-outbox dispatcher.
///
/// Split out of [`overlay`] so the durable-outbox knobs (default-off and inert
/// unless `outbox.enabled` is set) live beside one another and `overlay` stays
/// within the per-function line budget. Unknown names are ignored, exactly as
/// the `overlay` fallthrough does for every non-`AION_` variable.
fn overlay_outbox(config: &mut ServerConfig, name: &str, value: &str) -> Result<(), ServerError> {
    match name {
        "AION_OUTBOX_ENABLED" => {
            config.outbox.enabled = parse_bool(name, value)?;
        }
        "AION_OUTBOX_POLL_INTERVAL_MS" => {
            config.outbox.poll_interval_ms = Some(parse_positive_u64(name, value)?);
        }
        "AION_OUTBOX_BATCH_SIZE" => {
            config.outbox.batch_size = Some(parse_positive_u32(name, value)?);
        }
        "AION_OUTBOX_MAX_ATTEMPTS" => {
            config.outbox.max_attempts = Some(parse_positive_u32(name, value)?);
        }
        "AION_OUTBOX_BACKOFF_BASE_MS" => {
            config.outbox.backoff_base_ms = Some(parse_positive_u64(name, value)?);
        }
        "AION_OUTBOX_BACKOFF_MULTIPLIER" => {
            config.outbox.backoff_multiplier = Some(parse_positive_u32(name, value)?);
        }
        "AION_OUTBOX_BACKOFF_MAX_MS" => {
            config.outbox.backoff_max_ms = Some(parse_positive_u64(name, value)?);
        }
        "AION_OUTBOX_RECONCILE_INTERVAL_MS" => {
            config.outbox.reconcile_interval_ms = Some(parse_positive_u64(name, value)?);
        }
        "AION_OUTBOX_RECONCILE_STALE_AFTER_MS" => {
            config.outbox.reconcile_stale_after_ms = Some(parse_positive_u64(name, value)?);
        }
        "AION_OUTBOX_LIMINAL_LISTEN_ADDRESS" => {
            config.outbox.liminal_listen_address = Some(value.to_owned());
        }
        _ => {}
    }
    Ok(())
}

/// Parse a comma-separated `AION_SERVER_CORS_ALLOWED_ORIGINS` list into the
/// per-origin vector. Entries are trimmed and empties dropped, so an empty
/// value clears the list (back to the secure no-cross-origin default); the
/// resulting origins are validated for shape by `ServerConfig::validate`.
fn parse_csv_origins(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(str::to_owned)
        .collect()
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
        "haematite" => Ok(StoreBackend::Haematite),
        _ => config_error(format!("{name} must be one of: memory, libsql, haematite")),
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

fn parse_positive_u32(name: &str, value: &str) -> Result<u32, ServerError> {
    let parsed = value.parse::<u32>().map_err(|source| ServerError::Config {
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
