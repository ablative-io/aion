//! SS-1B: prove the haematite backend is a selectable `aion-server` store.
//!
//! These tests exercise the operator-facing config surface (`backend =
//! haematite`) and the production boot path that constructs `HaematiteStore`.
//! They are compiled only under the optional `haematite-backend` feature; a
//! default build neither links the backend nor runs these tests.
#![cfg(feature = "haematite-backend")]

use aion_server::config::{ServerConfig, StoreBackend};
use aion_server::state::ServerState;

/// A complete operator config selecting the haematite backend, pointed at
/// `data_dir`. The required query/streaming knobs are set so a full server boot
/// succeeds, proving the production path constructs `HaematiteStore`.
fn haematite_config_toml(data_dir: &str) -> String {
    format!(
        r#"
[server]
listen_address = "127.0.0.1:8080"
grpc_address = "127.0.0.1:50051"

[store]
backend = "haematite"
data_dir = "{data_dir}"
shard_count = 1

[runtime]
scheduler_threads = 1
query_timeout_ms = 10000

[websocket]
event_broadcast_capacity = 64
"#
    )
}

#[test]
fn haematite_config_parses_and_validates() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let toml = haematite_config_toml(&dir.path().to_string_lossy());
    let config = ServerConfig::from_slice(toml.as_bytes())?;
    assert_eq!(config.store.backend, StoreBackend::Haematite);
    assert_eq!(config.store.shard_count, 1);
    assert_eq!(
        config.store.data_dir.as_deref(),
        Some(dir.path().to_string_lossy().as_ref())
    );
    Ok(())
}

#[test]
fn haematite_config_rejects_missing_data_dir() -> Result<(), Box<dyn std::error::Error>> {
    let toml = r#"
[server]
listen_address = "127.0.0.1:8080"
grpc_address = "127.0.0.1:50051"

[store]
backend = "haematite"
"#;
    match ServerConfig::from_slice(toml.as_bytes()) {
        Ok(_) => Err("haematite backend without data_dir must be rejected".into()),
        Err(error) => {
            assert!(
                error.to_string().contains("data_dir"),
                "error should name the missing data_dir: {error}"
            );
            Ok(())
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn server_boots_over_haematite_backend() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let toml = haematite_config_toml(&dir.path().to_string_lossy());
    let config = ServerConfig::from_slice(toml.as_bytes())?;
    // Full production boot: this routes through `connect_store`, which constructs
    // a single-node `HaematiteStore` rooted at the temp data_dir and wraps it in
    // the instrumented/engine decorators exactly as for the other backends.
    let state = ServerState::build(config).await?;
    state.shutdown()?;
    Ok(())
}
