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
cluster_broadcast_capacity = 64
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
fn haematite_backend_defaults_data_dir_when_absent() -> Result<(), Box<dyn std::error::Error>> {
    // The ablative stack is the out-of-box durable default. Selecting
    // `backend = "haematite"` without a data_dir is valid and resolves under
    // Aion home rather than failing validation. (An EXPLICITLY empty
    // `data_dir = ""` is still rejected below.)
    let toml = r#"
[server]
listen_address = "127.0.0.1:8080"
grpc_address = "127.0.0.1:50051"

[store]
backend = "haematite"

[runtime]
query_timeout_ms = 10000

[websocket]
event_broadcast_capacity = 64
cluster_broadcast_capacity = 64
"#;
    let home = tempfile::tempdir()?;
    let config = ServerConfig::from_slice_with_home(toml.as_bytes(), home.path())?;
    assert_eq!(config.store.backend, StoreBackend::Haematite);
    assert_eq!(
        config.store.data_dir.as_deref(),
        home.path().join("data").to_str()
    );
    Ok(())
}

#[test]
fn haematite_config_rejects_empty_data_dir() -> Result<(), Box<dyn std::error::Error>> {
    // An explicitly empty data_dir is a misconfiguration, not "use the default":
    // the operator named the key, so validate() rejects the empty value.
    let toml = r#"
[server]
listen_address = "127.0.0.1:8080"
grpc_address = "127.0.0.1:50051"

[store]
backend = "haematite"
data_dir = ""

[runtime]
query_timeout_ms = 10000

[websocket]
event_broadcast_capacity = 64
cluster_broadcast_capacity = 64
"#;
    match ServerConfig::from_slice(toml.as_bytes()) {
        Ok(_) => Err("haematite backend with an empty data_dir must be rejected".into()),
        Err(error) => {
            assert!(
                error.to_string().contains("data_dir"),
                "error should name the empty data_dir: {error}"
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
    // No [cluster] section: the single-node path is taken, so the server is not
    // clustered (no distributed responder) — byte-identical to before SS-2.
    assert!(
        !state.is_clustered(),
        "a haematite boot without [store.cluster] must take the single-node path"
    );
    state.shutdown()?;
    Ok(())
}

/// SS-2: a complete operator config selecting the DISTRIBUTED haematite backend
/// as a "cluster of one" — `node_id` set, no peers, denominator 1. The boot path
/// binds the replication endpoint, elects the owned shard through the production
/// builder, and bootstraps the coordinator.
fn haematite_cluster_of_one_toml(data_dir: &str) -> String {
    format!(
        r#"
[server]
listen_address = "127.0.0.1:8080"
grpc_address = "127.0.0.1:50051"

[store]
backend = "haematite"
data_dir = "{data_dir}"
shard_count = 1
owned_shards = [0]

[store.cluster]
node_id = "ss2-server@127.0.0.1"
bind_address = "127.0.0.1:0"
members = ["ss2-server@127.0.0.1"]

[runtime]
scheduler_threads = 1
query_timeout_ms = 10000

[websocket]
event_broadcast_capacity = 64
cluster_broadcast_capacity = 64
"#
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn server_boots_as_a_cluster_of_one_and_elects_its_shard()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let toml = haematite_cluster_of_one_toml(&dir.path().to_string_lossy());
    let config = ServerConfig::from_slice(toml.as_bytes())?;
    // Full production boot of the DISTRIBUTED path: connect_store binds the
    // replication endpoint (off-runtime), the engine builder elects shard 0
    // (acquire_shard_and_serve, become_live) BEFORE recovery, and — owning the
    // only shard — bootstraps exactly one coordinator. A cluster-of-one election
    // self-quorums, so this is non-flaky.
    let state = ServerState::build(config).await?;
    assert!(
        state.is_clustered(),
        "a [store.cluster] section selects the distributed path with a live responder"
    );
    state.shutdown()?;
    Ok(())
}

#[test]
fn cluster_section_requires_haematite_backend() -> Result<(), Box<dyn std::error::Error>> {
    let toml = r#"
[server]
listen_address = "127.0.0.1:8080"
grpc_address = "127.0.0.1:50051"

[store]
backend = "libsql"
url = "file:test.db"

[store.cluster]
node_id = "n@127.0.0.1"
bind_address = "127.0.0.1:7000"

[runtime]
scheduler_threads = 1
query_timeout_ms = 10000

[websocket]
event_broadcast_capacity = 64
cluster_broadcast_capacity = 64
"#;
    match ServerConfig::from_slice(toml.as_bytes()) {
        Ok(_) => Err("[store.cluster] on a non-haematite backend must be rejected".into()),
        Err(error) => {
            assert!(
                error.to_string().contains("store.cluster"),
                "error should name store.cluster: {error}"
            );
            Ok(())
        }
    }
}
