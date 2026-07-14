//! Runtime-facing config views: [`RuntimeConfig`] and [`CliOverrides`].
//!
//! [`RuntimeConfig`] is the non-secret runtime slice carried in shared server
//! state for transport adapters (produced by [`ServerConfig::into_parts`]);
//! [`CliOverrides`] is the command-line override bundle merged after file and
//! environment values. Both are re-exported from the `config` module so every
//! existing `crate::config::X` path resolves identically.
//!
//! [`ServerConfig::into_parts`]: super::ServerConfig::into_parts

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use super::{
    AuthConfig, AuthoringConfig, AutoCreate, DeployConfig, DevConfig, ListenConfig, MetricsConfig,
    NamespaceConfig, ObservabilityConfig, OpsConsoleConfig, OutboxConfig, TlsConfig,
    WebSocketConfig, WorkerConfig,
};

/// Command-line configuration overrides applied after file and environment values.
#[derive(Debug, Default)]
pub struct CliOverrides {
    /// Optional explicit config path from `--config`.
    pub config_path: Option<PathBuf>,
    /// Override for `[server].listen_address`.
    pub listen_address: Option<SocketAddr>,
    /// Override for `[store].url`.
    pub store_url: Option<String>,
    /// Override for `[runtime].scheduler_threads`.
    pub scheduler_threads: Option<usize>,
    /// Override for `[drain].timeout_seconds`.
    pub drain_timeout_seconds: Option<u64>,
    /// Additional workflow package archives loaded after config and auto-discovered packages.
    pub workflow_packages: Vec<PathBuf>,
    /// Override for `[authoring].gleam_path`: the external `gleam` binary that
    /// gates the server-side authoring loop. Setting it commissions the
    /// authoring endpoints.
    pub gleam_path: Option<PathBuf>,
    /// Override for `[authoring].project_root`: the built Gleam workflow
    /// project submitted source is written into and packaged from.
    pub authoring_project_root: Option<PathBuf>,
}

/// Runtime settings retained in shared server state for transport adapters.
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// Listener addresses for public transports.
    pub listen: ListenConfig,
    /// Optional TLS material for public transports.
    pub tls: Option<TlsConfig>,
    /// Authentication configuration shared by transports.
    pub auth: AuthConfig,
    /// Ops-console asset location.
    pub ops_console: OpsConsoleConfig,
    /// Namespace resolver construction mode.
    pub namespace: NamespaceConfig,
    /// Remote worker heartbeat configuration.
    pub worker: WorkerConfig,
    /// WebSocket stream configuration.
    pub websocket: WebSocketConfig,
    /// Workflow package archives loaded into the engine at startup.
    pub workflow_packages: Vec<PathBuf>,
    /// Operator deploy API settings.
    pub deploy: DeployConfig,
    /// Server-side Gleam authoring API settings.
    pub authoring: AuthoringConfig,
    /// Local dev-server surface settings.
    pub dev: DevConfig,
    /// Durable-outbox fan-out dispatcher settings.
    pub outbox: OutboxConfig,
    /// Agent-observability transcript retention bounds (`[observability]`).
    pub observability: ObservabilityConfig,
    /// Engine scheduler thread count.
    pub scheduler_threads: usize,
    /// Engine reply deadline for workflow queries. REQUIRED — carried as an
    /// [`Option`] only so state construction can re-validate (defense in
    /// depth, like `websocket.event_broadcast_capacity`); validated
    /// configurations always hold [`Some`] non-zero duration.
    pub query_timeout: Option<Duration>,
    /// Default namespace used by worker dispatch and unauthenticated local callers.
    pub default_namespace: String,
    /// Minted-on-use policy applied at the worker-registration mint hook
    /// (`[namespaces] auto_create`). [`AutoCreate::Open`] (the default) mints an
    /// unseen namespace durably; [`AutoCreate::Closed`] rejects it.
    pub auto_create: AutoCreate,
    /// Platform-wide default for a namespace's cluster-wide concurrent
    /// in-flight-activity ceiling (`[namespaces] max_in_flight_activities`),
    /// applied when a namespace record carries no explicit override. Carried in
    /// runtime state so the later P2-Q2 keyed-backpressure dispatcher can read
    /// it without reloading config. Stored-only in this slice — nothing reads it
    /// yet.
    pub max_in_flight_activities: u32,
    /// Graceful drain timeout.
    pub drain_timeout: Duration,
    /// Metrics endpoint settings.
    pub metrics: MetricsConfig,
    /// Static distribution-shard assignment for this node (from `[store]
    /// owned_shards`). Empty means own ALL shards (single-node default,
    /// byte-identical to today); a non-empty set scopes engine recovery and
    /// enumeration to exactly those shards. No election: assignment is static.
    pub owned_shards: Vec<usize>,
    /// Browser origins allowed cross-origin access to the public HTTP API (from
    /// `[server] cors_allowed_origins`). Empty means no cross-origin access and
    /// no `CorsLayer` is installed (secure default); a non-empty set installs
    /// the layer scoped to exactly those origins.
    pub cors_allowed_origins: Vec<String>,
}
