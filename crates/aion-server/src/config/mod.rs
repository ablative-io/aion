//! Runtime configuration loading and validation for `aion-server`.

use std::{
    collections::HashSet,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;

use crate::error::ServerError;

/// Environment variable configuration loader.
pub mod env;
/// File-based configuration loader.
pub mod file;

const DEFAULT_HTTP_ADDRESS: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8080);
const DEFAULT_GRPC_ADDRESS: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 50051);

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

/// Complete merged server configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct ServerConfig {
    /// Public listener and transport addresses.
    pub server: ServerSection,
    /// Event-store backend configuration.
    pub store: StoreConfig,
    /// Engine runtime settings.
    pub runtime: RuntimeSection,
    /// Shutdown drain settings.
    pub drain: DrainConfig,
    /// Authentication settings defined by the operations config surface.
    pub auth: AuthConfig,
    /// Metrics endpoint settings.
    pub metrics: MetricsConfig,
    /// Namespace defaults.
    pub namespaces: NamespacesConfig,
    /// Optional TLS material for transports that require it.
    pub tls: Option<TlsConfig>,
    /// Static dashboard asset bundle location.
    pub dashboard: DashboardConfig,
    /// Namespace resolver construction mode retained for existing transports.
    pub namespace: NamespaceConfig,
    /// Remote-worker heartbeat policy.
    pub worker: WorkerConfig,
    /// WebSocket event streaming policy.
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
}

/// Public transport listener addresses from `[server]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSection {
    /// HTTP/JSON and dashboard listener.
    pub listen_address: SocketAddr,
    /// gRPC API and worker-protocol listener.
    pub grpc_address: SocketAddr,
}

/// Supported event-store backend names.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum StoreBackend {
    /// In-memory store for local development.
    Memory,
    /// libSQL durable store.
    LibSql,
}

/// Event-store backend configuration from `[store]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StoreConfig {
    /// Selected backing store implementation.
    pub backend: StoreBackend,
    /// Backend URL/path. For libSQL this is the embedded database path; for memory it is ignored.
    pub url: Option<String>,
}

/// Engine runtime settings from `[runtime]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeSection {
    /// Number of scheduler worker threads.
    pub scheduler_threads: usize,
    /// Engine reply deadline for workflow queries, in milliseconds.
    /// REQUIRED — the server always mounts `/workflows/query`, so the query
    /// reply deadline must be an explicit operator decision; there is no
    /// default. The engine builder is equally explicit-no-default.
    pub query_timeout_ms: Option<u64>,
}

/// Graceful drain settings from `[drain]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DrainConfig {
    /// Maximum drain duration in seconds.
    pub timeout_seconds: u64,
}

/// Authentication configuration applied at adapter boundaries.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    /// Whether authentication is enabled.
    pub enabled: bool,
    /// JWKS URL used by AO-006 auth validation.
    pub jwks_url: Option<String>,
    /// JWKS refresh interval in seconds.
    pub jwks_refresh_seconds: u64,
}

/// Metrics endpoint settings from `[metrics]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsConfig {
    /// Whether metrics are exposed.
    pub enabled: bool,
}

/// Namespace defaults from `[namespaces]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NamespacesConfig {
    /// Default namespace used for local callers and worker dispatch.
    pub default: String,
}

/// Public transport listener addresses retained for existing adapter code.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ListenConfig {
    /// gRPC API and worker-protocol listener.
    pub grpc: SocketAddr,
    /// HTTP/JSON and dashboard listener.
    pub http: SocketAddr,
}

/// TLS certificate and private-key material.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// Certificate chain path supplied by the operator.
    pub certificate_chain_path: PathBuf,
    /// Private-key path supplied by the operator.
    pub private_key_path: PathBuf,
}

/// Static dashboard asset configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DashboardConfig {
    /// Operator-selected bundle source.
    pub source: DashboardAssetSource,
}

/// Static dashboard bundle source.
#[derive(Clone, Debug, Deserialize)]
pub enum DashboardAssetSource {
    /// Serve the built bundle from an operator-supplied directory.
    FileSystem {
        /// Directory containing `index.html` and built asset files.
        asset_path: PathBuf,
    },
    /// Serve the compile-time embedded bundle.
    Embedded,
}

/// Namespace resolver construction mode.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NamespaceConfig {
    /// Deployment-selected namespace mapping mode.
    pub mode: NamespaceMode,
}

/// Supported namespace mapping modes.
#[derive(Clone, Debug, Deserialize)]
pub enum NamespaceMode {
    /// All authorized namespaces share the configured engine instance.
    SharedEngine,
    /// Namespace authorization is disabled only for single-tenant deployments.
    SingleTenant {
        /// The only namespace accepted by the deployment.
        namespace: String,
    },
}

/// Remote worker heartbeat configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkerConfig {
    /// Window after which a silent worker is considered lost.
    #[serde(with = "duration_millis")]
    pub heartbeat_window: Duration,
}

/// WebSocket stream configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebSocketConfig {
    /// Per-connection outbound buffer bound.
    pub outbound_buffer_bound: usize,
    /// Capacity of the engine-global event broadcast channel that backs
    /// `/events/stream`. REQUIRED — the server always mounts the streaming
    /// endpoint, so streaming capacity must be an explicit operator decision;
    /// there is no default. Lag is filter-blind, so size this for global event
    /// volume across all namespaces, not per-subscription volume.
    pub event_broadcast_capacity: Option<usize>,
}

/// Operator-facing message for an absent or zero `event_broadcast_capacity`.
pub(crate) const EVENT_BROADCAST_CAPACITY_REQUIRED: &str = "websocket.event_broadcast_capacity is required and has no default: the server always mounts /events/stream, so live event streaming capacity must be configured explicitly; set websocket.event_broadcast_capacity (or AION_WEBSOCKET_EVENT_BROADCAST_CAPACITY) to a positive integer sized for global event volume across all namespaces";

/// Operator deploy API settings from `[deploy]`.
///
/// The deploy surface is dark by default: with `enabled = false` (or the
/// section absent) neither the `/deploy/*` HTTP routes nor the gRPC
/// `DeployService` are mounted, so a workflow server that is not a deploy
/// target exposes no deploy attack surface at all.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeployConfig {
    /// Whether the deploy surface is mounted. Defaults to false.
    pub enabled: bool,
    /// Upload-size ceiling for `.aion` archives, in bytes. REQUIRED when
    /// `enabled = true`; no default (house rule) — the operator sizes it for
    /// their packages.
    pub max_archive_bytes: Option<u64>,
    /// Inflate ceiling for uploaded archive contents, in bytes: the total
    /// decompressed size of all archive entries an upload may extract to
    /// (DEFLATE bombs inflate ~1000:1 past `max_archive_bytes`). REQUIRED
    /// when `enabled = true`; no default (house rule); must be at least
    /// `max_archive_bytes`.
    pub max_inflated_bytes: Option<u64>,
}

/// Operator-facing message for an absent or zero `deploy.max_archive_bytes`.
pub(crate) const DEPLOY_MAX_ARCHIVE_BYTES_REQUIRED: &str = "deploy.max_archive_bytes is required and has no default when deploy.enabled is true: the archive upload ceiling must be an explicit operator decision sized for the deployment's packages; set deploy.max_archive_bytes (or AION_DEPLOY_MAX_ARCHIVE_BYTES) to a positive number of bytes";

/// Operator-facing message for an absent or zero `deploy.max_inflated_bytes`.
pub(crate) const DEPLOY_MAX_INFLATED_BYTES_REQUIRED: &str = "deploy.max_inflated_bytes is required and has no default when deploy.enabled is true: the decompressed-contents ceiling for uploaded archives must be an explicit operator decision (a compressed upload under deploy.max_archive_bytes can inflate ~1000:1); set deploy.max_inflated_bytes (or AION_DEPLOY_MAX_INFLATED_BYTES) to a positive number of bytes no smaller than deploy.max_archive_bytes";

/// Operator-facing message for an absent or zero `query_timeout_ms`.
pub(crate) const QUERY_TIMEOUT_REQUIRED: &str = "runtime.query_timeout_ms is required and has no default: the server always mounts /workflows/query, so the workflow query reply deadline must be configured explicitly; set runtime.query_timeout_ms (or AION_RUNTIME_QUERY_TIMEOUT_MS) to a positive number of milliseconds";

/// Local dev-server surface settings from `[dev]`.
///
/// The dev surface is dark by default, gated on `enabled`: with it false (the
/// section absent or `enabled = false`) the `/dev/*` routes are not mounted,
/// the engine installs the bare production activity dispatcher (no mocking
/// decorator), and nothing dev-specific is ever reachable. Setting `enabled =
/// true` mounts the dev endpoints and installs the per-run activity-mock
/// decorator — a development affordance, never on in production. It adds no
/// arbitrary defaults (ADR-001): the only knob is the on/off gate.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DevConfig {
    /// Whether the local dev-server surface is mounted. Defaults to false.
    pub enabled: bool,
}

/// Durable-outbox fan-out dispatcher settings from `[outbox]`.
///
/// The outbox dispatcher is dark by default, gated on `enabled`: with it false
/// (the section absent or `enabled = false`) the non-replayed background task
/// that claims pending outbox rows and dispatches them to connected workers is
/// never spawned, so default server behaviour is unchanged and the live
/// workflow dispatch path is the only dispatch path. Setting `enabled = true`
/// commissions the dispatcher and makes every operational knob below REQUIRED —
/// poll interval, claim batch size, retry budget, and the backoff curve all
/// come from explicit operator decisions (ADR-001: no assumed defaults).
///
/// Scope: this Phase-2 dispatcher dispatches claimed rows and marks each row's
/// terminal outbox state (done / retry / failed). Routing the worker completion
/// back into workflow history through the Recorder is Phase 3 and is not wired
/// here; with the flag off there is no behavioural difference at all.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutboxConfig {
    /// Whether the outbox dispatcher background task is spawned. Defaults to
    /// false, leaving the dispatcher dark and server behaviour unchanged.
    pub enabled: bool,
    /// Interval between successive claim sweeps, in milliseconds. REQUIRED when
    /// `enabled = true`; no default (house rule) — the operator sizes the poll
    /// cadence for their fan-out volume and latency budget.
    pub poll_interval_ms: Option<u64>,
    /// Maximum number of pending rows claimed per sweep. REQUIRED when
    /// `enabled = true`; no default (house rule).
    pub batch_size: Option<u32>,
    /// Dispatch attempts before a row is dead-lettered to `failed`. REQUIRED
    /// when `enabled = true`; no default (house rule). Must be at least one.
    pub max_attempts: Option<u32>,
    /// Base retry backoff applied to the first retry, in milliseconds. REQUIRED
    /// when `enabled = true`; no default (house rule). Successive retries
    /// multiply this by `backoff_multiplier` raised to the prior-attempt count,
    /// capped at `backoff_max_ms`.
    pub backoff_base_ms: Option<u64>,
    /// Geometric growth factor applied to the backoff per prior attempt.
    /// REQUIRED when `enabled = true`; no default (house rule). Must be at
    /// least one so backoff never shrinks.
    pub backoff_multiplier: Option<u32>,
    /// Upper bound on a single retry's backoff, in milliseconds. REQUIRED when
    /// `enabled = true`; no default (house rule). Must be at least
    /// `backoff_base_ms`.
    pub backoff_max_ms: Option<u64>,
}

/// Operator-facing message for an absent or zero `outbox.poll_interval_ms`.
pub(crate) const OUTBOX_POLL_INTERVAL_REQUIRED: &str = "outbox.poll_interval_ms is required and has no default when outbox.enabled is true: the dispatcher claim cadence must be an explicit operator decision sized for fan-out volume and latency; set outbox.poll_interval_ms (or AION_OUTBOX_POLL_INTERVAL_MS) to a positive number of milliseconds";

/// Operator-facing message for an absent or zero `outbox.batch_size`.
pub(crate) const OUTBOX_BATCH_SIZE_REQUIRED: &str = "outbox.batch_size is required and has no default when outbox.enabled is true: the per-sweep claim ceiling must be an explicit operator decision; set outbox.batch_size (or AION_OUTBOX_BATCH_SIZE) to a positive integer";

/// Operator-facing message for an absent or zero `outbox.max_attempts`.
pub(crate) const OUTBOX_MAX_ATTEMPTS_REQUIRED: &str = "outbox.max_attempts is required and has no default when outbox.enabled is true: the dispatch retry budget before dead-lettering must be an explicit operator decision; set outbox.max_attempts (or AION_OUTBOX_MAX_ATTEMPTS) to a positive integer";

/// Operator-facing message for an absent or zero `outbox.backoff_base_ms`.
pub(crate) const OUTBOX_BACKOFF_BASE_REQUIRED: &str = "outbox.backoff_base_ms is required and has no default when outbox.enabled is true: the first-retry backoff must be an explicit operator decision; set outbox.backoff_base_ms (or AION_OUTBOX_BACKOFF_BASE_MS) to a positive number of milliseconds";

/// Operator-facing message for an absent or zero `outbox.backoff_multiplier`.
pub(crate) const OUTBOX_BACKOFF_MULTIPLIER_REQUIRED: &str = "outbox.backoff_multiplier is required and has no default when outbox.enabled is true: the geometric backoff growth factor must be an explicit operator decision and must be at least one so backoff never shrinks; set outbox.backoff_multiplier (or AION_OUTBOX_BACKOFF_MULTIPLIER) to a positive integer";

/// Operator-facing message for an absent or undersized `outbox.backoff_max_ms`.
pub(crate) const OUTBOX_BACKOFF_MAX_REQUIRED: &str = "outbox.backoff_max_ms is required and has no default when outbox.enabled is true and must be at least outbox.backoff_base_ms: the per-retry backoff ceiling must be an explicit operator decision; set outbox.backoff_max_ms (or AION_OUTBOX_BACKOFF_MAX_MS) to a positive number of milliseconds no smaller than outbox.backoff_base_ms";

/// Server-side Gleam authoring API settings from `[authoring]`.
///
/// The authoring surface is dark by default, gated on `gleam_path`: with no
/// `gleam_path` set (the section absent or `gleam_path` unset) the
/// `/authoring/*` routes are not mounted, the server deploys pre-built `.aion`
/// files only, and nothing ever invokes `gleam` (CN7). Setting `gleam_path`
/// commissions the authoring loop and makes `project_root` required — the
/// built Gleam project submitted source is written into and packaged from.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthoringConfig {
    /// Path to the external `gleam` binary the toolchain spawns. `None`
    /// (the default) leaves the authoring surface dark; setting it gates the
    /// `/authoring/*` endpoints on. There is no default binary — the operator
    /// names it explicitly.
    pub gleam_path: Option<PathBuf>,
    /// Built Gleam workflow project root submitted source is written into and
    /// packaged from. REQUIRED when `gleam_path` is set; no default (house
    /// rule) — a Gleam project needs `gleam.toml`, the `aion_flow` dependency,
    /// `workflow.toml`, and `schemas/`, so the operator provisions and names
    /// the project root.
    pub project_root: Option<PathBuf>,
}

/// Operator-facing message for an absent or empty `authoring.gleam_path` value.
pub(crate) const AUTHORING_GLEAM_PATH_EMPTY: &str = "authoring.gleam_path must not be empty when set: it names the external gleam binary the authoring loop spawns; set authoring.gleam_path (or AION_AUTHORING_GLEAM_PATH) to the path of a runnable gleam binary, or remove it to leave the authoring surface dark";

/// Operator-facing message for an absent `authoring.project_root` when the
/// authoring surface is commissioned.
pub(crate) const AUTHORING_PROJECT_ROOT_REQUIRED: &str = "authoring.project_root is required and has no default when authoring.gleam_path is set: submitted Gleam source is written into and packaged from a built project, so the operator must provision and name the project root (a directory with gleam.toml, the aion_flow dependency, workflow.toml, and schemas/); set authoring.project_root (or AION_AUTHORING_PROJECT_ROOT)";

/// Runtime settings retained in shared server state for transport adapters.
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// Listener addresses for public transports.
    pub listen: ListenConfig,
    /// Optional TLS material for public transports.
    pub tls: Option<TlsConfig>,
    /// Authentication configuration shared by transports.
    pub auth: AuthConfig,
    /// Dashboard asset location.
    pub dashboard: DashboardConfig,
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
    /// Engine scheduler thread count.
    pub scheduler_threads: usize,
    /// Engine reply deadline for workflow queries. REQUIRED — carried as an
    /// [`Option`] only so state construction can re-validate (defense in
    /// depth, like `websocket.event_broadcast_capacity`); validated
    /// configurations always hold [`Some`] non-zero duration.
    pub query_timeout: Option<Duration>,
    /// Default namespace used by worker dispatch and unauthenticated local callers.
    pub default_namespace: String,
    /// Graceful drain timeout.
    pub drain_timeout: Duration,
    /// Metrics endpoint settings.
    pub metrics: MetricsConfig,
}

impl ServerConfig {
    /// Load and merge config from defaults, optional TOML file, environment, and CLI overrides.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Config`] when file discovery, parsing, environment parsing, CLI
    /// values, or validation fail.
    pub fn load(cli: &CliOverrides) -> Result<Self, ServerError> {
        let mut config = file::load(cli.config_path.as_deref())?.unwrap_or_default();
        env::overlay(&mut config)?;
        config.apply_cli_overrides(cli);
        config.load_discovered_workflow_packages(cli, Path::new("."))?;
        config.validate()?;
        Ok(config)
    }

    fn load_discovered_workflow_packages(
        &mut self,
        cli: &CliOverrides,
        directory: &Path,
    ) -> Result<(), ServerError> {
        let discovered_packages = discover_workflow_packages(directory)?;
        merge_workflow_packages(
            &mut self.workflow_packages,
            discovered_packages,
            &cli.workflow_packages,
        );
        Ok(())
    }

    /// Parse server configuration from TOML bytes and validate it.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Config`] when parsing fails or values are invalid.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, ServerError> {
        let config: Self = toml::from_slice(bytes).map_err(|source| ServerError::Config {
            message: format!("invalid server config: {source}"),
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Load server configuration from an explicit TOML file path.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Config`] when the file is missing, unreadable, unparsable, or invalid.
    pub fn load_from_path(path: impl Into<PathBuf>) -> Result<Self, ServerError> {
        file::load_required(&path.into())
    }

    /// Split store configuration from non-secret runtime settings.
    #[must_use]
    pub fn into_parts(self) -> (StoreConfig, RuntimeConfig) {
        let runtime = RuntimeConfig {
            listen: ListenConfig {
                grpc: self.server.grpc_address,
                http: self.server.listen_address,
            },
            tls: self.tls,
            auth: self.auth,
            dashboard: self.dashboard,
            namespace: self.namespace,
            worker: self.worker,
            websocket: self.websocket,
            workflow_packages: self.workflow_packages,
            deploy: self.deploy,
            authoring: self.authoring,
            dev: self.dev,
            outbox: self.outbox,
            scheduler_threads: self.runtime.scheduler_threads,
            query_timeout: self.runtime.query_timeout_ms.map(Duration::from_millis),
            default_namespace: self.namespaces.default,
            drain_timeout: Duration::from_secs(self.drain.timeout_seconds),
            metrics: self.metrics,
        };
        (self.store, runtime)
    }

    fn apply_cli_overrides(&mut self, cli: &CliOverrides) {
        if let Some(address) = cli.listen_address {
            self.server.listen_address = address;
        }
        if let Some(url) = &cli.store_url {
            self.store.url = Some(url.clone());
            if self.store.backend == StoreBackend::Memory {
                self.store.backend = StoreBackend::LibSql;
            }
        }
        if let Some(threads) = cli.scheduler_threads {
            self.runtime.scheduler_threads = threads;
        }
        if let Some(timeout) = cli.drain_timeout_seconds {
            self.drain.timeout_seconds = timeout;
        }
        if let Some(gleam_path) = &cli.gleam_path {
            self.authoring.gleam_path = Some(gleam_path.clone());
        }
        if let Some(project_root) = &cli.authoring_project_root {
            self.authoring.project_root = Some(project_root.clone());
        }
    }

    fn validate(&self) -> Result<(), ServerError> {
        if self.server.listen_address.port() == 0 {
            return config_error("server.listen_address must use an explicit non-zero port");
        }
        if self.server.grpc_address.port() == 0 {
            return config_error("server.grpc_address must use an explicit non-zero port");
        }
        if self.runtime.scheduler_threads == 0 {
            return config_error("runtime.scheduler_threads must be greater than zero");
        }
        if self.drain.timeout_seconds == 0 {
            return config_error("drain.timeout_seconds must be greater than zero");
        }
        if self.auth.enabled && self.auth.jwks_url.as_deref().is_none_or(str::is_empty) {
            return config_error("auth.jwks_url must not be empty when auth.enabled is true");
        }
        if self.auth.jwks_refresh_seconds == 0 {
            return config_error("auth.jwks_refresh_seconds must be greater than zero");
        }
        if self.namespaces.default.is_empty() {
            return config_error("namespaces.default must not be empty");
        }
        if matches!(self.store.backend, StoreBackend::LibSql)
            && self.store.url.as_deref().is_none_or(str::is_empty)
        {
            return config_error("store.url must not be empty when store.backend is libsql");
        }
        if let Some(url) = &self.store.url {
            if url.is_empty() {
                return config_error("store.url must not be empty");
            }
        }
        if let DashboardAssetSource::FileSystem { asset_path } = &self.dashboard.source {
            if asset_path.as_os_str().is_empty() {
                return config_error("dashboard.source.FileSystem.asset_path must not be empty");
            }
        }
        if let NamespaceMode::SingleTenant { namespace } = &self.namespace.mode {
            if namespace.is_empty() {
                return config_error("namespace.mode.SingleTenant.namespace must not be empty");
            }
        }
        if self.worker.heartbeat_window.is_zero() {
            return config_error("worker.heartbeat_window must be greater than zero");
        }
        if self.websocket.outbound_buffer_bound == 0 {
            return config_error("websocket.outbound_buffer_bound must be greater than zero");
        }
        match self.websocket.event_broadcast_capacity {
            None | Some(0) => return config_error(EVENT_BROADCAST_CAPACITY_REQUIRED),
            Some(_) => {}
        }
        match self.runtime.query_timeout_ms {
            None | Some(0) => return config_error(QUERY_TIMEOUT_REQUIRED),
            Some(_) => {}
        }
        if self.deploy.enabled {
            let max_archive_bytes = match self.deploy.max_archive_bytes {
                None | Some(0) => return config_error(DEPLOY_MAX_ARCHIVE_BYTES_REQUIRED),
                Some(value) => value,
            };
            let max_inflated_bytes = match self.deploy.max_inflated_bytes {
                None | Some(0) => return config_error(DEPLOY_MAX_INFLATED_BYTES_REQUIRED),
                Some(value) => value,
            };
            // Both ceilings size in-memory buffers, so they must be
            // addressable on this platform (32-bit targets).
            ensure_fits_usize("deploy.max_archive_bytes", max_archive_bytes)?;
            ensure_fits_usize("deploy.max_inflated_bytes", max_inflated_bytes)?;
            if max_inflated_bytes < max_archive_bytes {
                return config_error(format!(
                    "deploy.max_inflated_bytes ({max_inflated_bytes}) must be at least deploy.max_archive_bytes ({max_archive_bytes}): an inflate ceiling below the upload ceiling would refuse archives the upload ceiling admits, even stored uncompressed"
                ));
            }
        }
        if let Some(gleam_path) = &self.authoring.gleam_path {
            // The authoring surface is commissioned by a non-empty gleam_path;
            // an empty value is a misconfiguration, not "dark".
            if gleam_path.as_os_str().is_empty() {
                return config_error(AUTHORING_GLEAM_PATH_EMPTY);
            }
            // Commissioning the loop requires a project root with no default
            // (a Gleam project cannot be invented; the operator provisions it).
            match &self.authoring.project_root {
                Some(root) if !root.as_os_str().is_empty() => {}
                _ => return config_error(AUTHORING_PROJECT_ROOT_REQUIRED),
            }
        }
        self.validate_outbox()?;
        Ok(())
    }

    /// Validate the durable-outbox dispatcher knobs.
    ///
    /// All knobs are inert while `outbox.enabled` is false (the dispatcher is
    /// never spawned), so they are only required — and only checked — once the
    /// operator commissions the dispatcher. This mirrors the dark-by-default
    /// `deploy` surface: the on/off gate carries no defaults, and every
    /// operational value behind it is an explicit operator decision.
    fn validate_outbox(&self) -> Result<(), ServerError> {
        if !self.outbox.enabled {
            return Ok(());
        }
        match self.outbox.poll_interval_ms {
            None | Some(0) => return config_error(OUTBOX_POLL_INTERVAL_REQUIRED),
            Some(_) => {}
        }
        match self.outbox.batch_size {
            None | Some(0) => return config_error(OUTBOX_BATCH_SIZE_REQUIRED),
            Some(_) => {}
        }
        match self.outbox.max_attempts {
            None | Some(0) => return config_error(OUTBOX_MAX_ATTEMPTS_REQUIRED),
            Some(_) => {}
        }
        let backoff_base_ms = match self.outbox.backoff_base_ms {
            None | Some(0) => return config_error(OUTBOX_BACKOFF_BASE_REQUIRED),
            Some(value) => value,
        };
        match self.outbox.backoff_multiplier {
            None | Some(0) => return config_error(OUTBOX_BACKOFF_MULTIPLIER_REQUIRED),
            Some(_) => {}
        }
        match self.outbox.backoff_max_ms {
            Some(max) if max >= backoff_base_ms => {}
            _ => return config_error(OUTBOX_BACKOFF_MAX_REQUIRED),
        }
        Ok(())
    }
}

/// Refuses byte-ceiling values that cannot index memory on this platform.
fn ensure_fits_usize(key: &str, value: u64) -> Result<(), ServerError> {
    if usize::try_from(value).is_err() {
        return config_error(format!(
            "{key} ({value}) exceeds this platform's addressable memory; set it to at most {}",
            usize::MAX
        ));
    }
    Ok(())
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            listen_address: DEFAULT_HTTP_ADDRESS,
            grpc_address: DEFAULT_GRPC_ADDRESS,
        }
    }
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            backend: StoreBackend::Memory,
            url: None,
        }
    }
}

impl Default for RuntimeSection {
    fn default() -> Self {
        Self {
            scheduler_threads: 1,
            // Deliberately absent: validation fails loudly until the operator
            // sets the workflow query reply deadline for the deployment.
            query_timeout_ms: None,
        }
    }
}

impl Default for DrainConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 30,
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            jwks_url: None,
            jwks_refresh_seconds: 300,
        }
    }
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Default for NamespacesConfig {
    fn default() -> Self {
        Self {
            default: "default".to_owned(),
        }
    }
}

impl Default for ListenConfig {
    fn default() -> Self {
        Self {
            grpc: DEFAULT_GRPC_ADDRESS,
            http: DEFAULT_HTTP_ADDRESS,
        }
    }
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            source: DashboardAssetSource::Embedded,
        }
    }
}

impl Default for NamespaceConfig {
    fn default() -> Self {
        Self {
            mode: NamespaceMode::SharedEngine,
        }
    }
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            heartbeat_window: Duration::from_secs(30),
        }
    }
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            outbound_buffer_bound: 32,
            // Deliberately absent: validation fails loudly until the operator
            // sizes the engine-global broadcast channel for the deployment.
            event_broadcast_capacity: None,
        }
    }
}

pub(crate) fn config_error<T>(message: impl Into<String>) -> Result<T, ServerError> {
    Err(ServerError::Config {
        message: message.into(),
    })
}

fn discover_workflow_packages(directory: &Path) -> Result<Vec<PathBuf>, ServerError> {
    let mut packages = Vec::new();
    let entries = fs::read_dir(directory).map_err(|source| ServerError::Config {
        message: format!(
            "failed to scan workflow packages in `{}`: {source}",
            directory.display()
        ),
    })?;

    for entry in entries {
        let entry = entry.map_err(|source| ServerError::Config {
            message: format!(
                "failed to read workflow package entry in `{}`: {source}",
                directory.display()
            ),
        })?;
        let path = entry.path();
        let has_aion_extension = path
            .extension()
            .is_some_and(|extension| extension == "aion");
        if path.is_file() && has_aion_extension {
            packages.push(path);
        }
    }

    packages.sort_by(|left, right| left.as_os_str().cmp(right.as_os_str()));
    Ok(packages)
}

fn merge_workflow_packages(
    workflow_packages: &mut Vec<PathBuf>,
    discovered_packages: Vec<PathBuf>,
    cli_packages: &[PathBuf],
) {
    let mut seen: HashSet<PathBuf> = workflow_packages
        .iter()
        .map(|package| deduplicated_package_key(package))
        .collect();
    for package in discovered_packages
        .into_iter()
        .chain(cli_packages.iter().cloned())
    {
        if seen.insert(deduplicated_package_key(&package)) {
            workflow_packages.push(package);
        }
    }
}

fn deduplicated_package_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

mod duration_millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer};

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let millis = u64::deserialize(deserializer)?;
        Ok(Duration::from_millis(millis))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CliOverrides, ServerConfig, StoreBackend, discover_workflow_packages,
        merge_workflow_packages,
    };

    #[test]
    fn valid_toml_is_parsed_into_typed_config() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br#"
                [server]
                listen_address = "127.0.0.1:18080"
                grpc_address = "127.0.0.1:15051"

                [store]
                backend = "libsql"
                url = "aion.db"

                [runtime]
                scheduler_threads = 2
                query_timeout_ms = 10000

                [drain]
                timeout_seconds = 45

                [auth]
                enabled = true
                jwks_url = "https://issuer.example.com/.well-known/jwks.json"
                jwks_refresh_seconds = 60

                [metrics]
                enabled = true

                [namespaces]
                default = "production"

                [websocket]
                outbound_buffer_bound = 16
                event_broadcast_capacity = 1024
            "#,
        )?;

        assert_eq!(config.store.backend, StoreBackend::LibSql);
        assert_eq!(config.store.url.as_deref(), Some("aion.db"));
        assert_eq!(config.runtime.scheduler_threads, 2);
        assert_eq!(config.runtime.query_timeout_ms, Some(10_000));
        assert_eq!(config.namespaces.default, "production");
        assert_eq!(config.websocket.outbound_buffer_bound, 16);
        assert_eq!(config.websocket.event_broadcast_capacity, Some(1024));
        Ok(())
    }

    #[test]
    fn missing_event_broadcast_capacity_fails_startup_validation_naming_the_key() {
        // The server unconditionally mounts /events/stream; a configuration
        // without explicit broadcast capacity must fail loudly at startup
        // instead of leaving streaming dark.
        let result = ServerConfig::default().validate();

        let message = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        assert!(
            message.contains("websocket.event_broadcast_capacity"),
            "validation message must name the missing key: {message}"
        );
        assert!(
            message.contains("AION_WEBSOCKET_EVENT_BROADCAST_CAPACITY"),
            "validation message must name the environment override: {message}"
        );
    }

    #[test]
    fn zero_event_broadcast_capacity_fails_startup_validation() {
        let result = ServerConfig::from_slice(
            br"
                [websocket]
                event_broadcast_capacity = 0
            ",
        );

        let message = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        assert!(
            message.contains("websocket.event_broadcast_capacity"),
            "validation message must name the zero-valued key: {message}"
        );
    }

    #[test]
    fn missing_query_timeout_fails_startup_validation_naming_the_key() {
        // The server unconditionally mounts /workflows/query; a configuration
        // without an explicit query reply deadline must fail loudly at
        // startup instead of mounting an unanswerable surface.
        let result = ServerConfig::from_slice(
            br"
                [runtime]
                scheduler_threads = 1

                [websocket]
                event_broadcast_capacity = 64
            ",
        );

        let message = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        assert!(
            message.contains("runtime.query_timeout_ms"),
            "validation message must name the missing key: {message}"
        );
        assert!(
            message.contains("AION_RUNTIME_QUERY_TIMEOUT_MS"),
            "validation message must name the environment override: {message}"
        );
    }

    #[test]
    fn zero_query_timeout_fails_startup_validation() {
        let result = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 0

                [websocket]
                event_broadcast_capacity = 64
            ",
        );

        let message = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        assert!(
            message.contains("runtime.query_timeout_ms"),
            "validation message must name the zero-valued key: {message}"
        );
    }

    /// The deploy surface is commissioned explicitly: enabling it without
    /// the archive ceiling must fail startup naming the key and the
    /// environment override (the `query_timeout_ms` /
    /// `event_broadcast_capacity` required-config pattern).
    #[test]
    fn deploy_enabled_without_max_archive_bytes_fails_naming_key_and_env() {
        let result = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64

                [deploy]
                enabled = true
            ",
        );

        let message = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        assert!(
            message.contains("deploy.max_archive_bytes"),
            "validation message must name the missing key: {message}"
        );
        assert!(
            message.contains("AION_DEPLOY_MAX_ARCHIVE_BYTES"),
            "validation message must name the environment override: {message}"
        );
    }

    #[test]
    fn deploy_zero_max_archive_bytes_fails_startup_validation() {
        let result = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64

                [deploy]
                enabled = true
                max_archive_bytes = 0
            ",
        );

        let message = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        assert!(
            message.contains("deploy.max_archive_bytes"),
            "validation message must name the zero-valued key: {message}"
        );
    }

    /// The inflate ceiling is commissioned alongside the upload ceiling:
    /// enabling deploy without `max_inflated_bytes` must fail startup naming
    /// the key and the environment override (same pattern as
    /// `max_archive_bytes`).
    #[test]
    fn deploy_enabled_without_max_inflated_bytes_fails_naming_key_and_env() {
        let result = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64

                [deploy]
                enabled = true
                max_archive_bytes = 16777216
            ",
        );

        let message = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        assert!(
            message.contains("deploy.max_inflated_bytes"),
            "validation message must name the missing key: {message}"
        );
        assert!(
            message.contains("AION_DEPLOY_MAX_INFLATED_BYTES"),
            "validation message must name the environment override: {message}"
        );
    }

    #[test]
    fn deploy_zero_max_inflated_bytes_fails_startup_validation() {
        let result = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64

                [deploy]
                enabled = true
                max_archive_bytes = 16777216
                max_inflated_bytes = 0
            ",
        );

        let message = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        assert!(
            message.contains("deploy.max_inflated_bytes"),
            "validation message must name the zero-valued key: {message}"
        );
    }

    /// An inflate ceiling below the upload ceiling is incoherent: archives
    /// the upload ceiling admits would be refused even stored uncompressed.
    #[test]
    fn deploy_max_inflated_below_max_archive_fails_startup_validation() {
        let result = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64

                [deploy]
                enabled = true
                max_archive_bytes = 16777216
                max_inflated_bytes = 16777215
            ",
        );

        let message = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        assert!(
            message.contains("deploy.max_inflated_bytes")
                && message.contains("deploy.max_archive_bytes"),
            "validation message must name both ceilings: {message}"
        );
    }

    /// An absent `[deploy]` section means the surface stays dark and the
    /// ceilings are not required.
    #[test]
    fn deploy_disabled_requires_no_archive_ceiling() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
            ",
        )?;

        assert!(!config.deploy.enabled);
        assert_eq!(config.deploy.max_archive_bytes, None);
        assert_eq!(config.deploy.max_inflated_bytes, None);
        Ok(())
    }

    #[test]
    fn deploy_section_parses_enabled_with_ceilings() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64

                [deploy]
                enabled = true
                max_archive_bytes = 16777216
                max_inflated_bytes = 67108864
            ",
        )?;

        assert!(config.deploy.enabled);
        assert_eq!(config.deploy.max_archive_bytes, Some(16_777_216));
        assert_eq!(config.deploy.max_inflated_bytes, Some(67_108_864));
        Ok(())
    }

    /// An absent `[dev]` section leaves the dev surface dark.
    #[test]
    fn dev_absent_leaves_surface_dark() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
            ",
        )?;

        assert!(!config.dev.enabled);
        Ok(())
    }

    /// `[dev] enabled = true` commissions the dev surface; it adds no other
    /// knobs (ADR-001: the only setting is the on/off gate).
    #[test]
    fn dev_section_parses_enabled() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64

                [dev]
                enabled = true
            ",
        )?;

        assert!(config.dev.enabled);
        Ok(())
    }

    /// An absent `[authoring]` section leaves the surface dark: no `gleam_path`,
    /// no `project_root`, and validation does not require either.
    #[test]
    fn authoring_absent_leaves_surface_dark() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
            ",
        )?;

        assert_eq!(config.authoring.gleam_path, None);
        assert_eq!(config.authoring.project_root, None);
        Ok(())
    }

    /// A configured `[authoring]` section with both `gleam_path` and
    /// `project_root` parses and round-trips into `RuntimeConfig`.
    #[test]
    fn authoring_section_parses_and_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br#"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64

                [authoring]
                gleam_path = "/usr/local/bin/gleam"
                project_root = "/srv/aion/authoring"
            "#,
        )?;

        assert_eq!(
            config.authoring.gleam_path.as_deref(),
            Some(std::path::Path::new("/usr/local/bin/gleam"))
        );
        let (_, runtime) = config.into_parts();
        assert_eq!(
            runtime.authoring.gleam_path.as_deref(),
            Some(std::path::Path::new("/usr/local/bin/gleam"))
        );
        assert_eq!(
            runtime.authoring.project_root.as_deref(),
            Some(std::path::Path::new("/srv/aion/authoring"))
        );
        Ok(())
    }

    /// Commissioning the authoring loop (a `gleam_path`) without a
    /// `project_root` must fail startup naming the key and the environment
    /// override (the deploy required-config pattern).
    #[test]
    fn authoring_gleam_path_without_project_root_fails_naming_key_and_env() {
        let result = ServerConfig::from_slice(
            br#"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64

                [authoring]
                gleam_path = "/usr/local/bin/gleam"
            "#,
        );

        let message = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        assert!(
            message.contains("authoring.project_root"),
            "validation message must name the missing key: {message}"
        );
        assert!(
            message.contains("AION_AUTHORING_PROJECT_ROOT"),
            "validation message must name the environment override: {message}"
        );
    }

    /// An empty `gleam_path` is a misconfiguration, not "dark": it must fail
    /// startup naming the key and the environment override.
    #[test]
    fn authoring_empty_gleam_path_fails_naming_key_and_env() {
        let result = ServerConfig::from_slice(
            br#"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64

                [authoring]
                gleam_path = ""
            "#,
        );

        let message = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        assert!(
            message.contains("authoring.gleam_path"),
            "validation message must name the empty key: {message}"
        );
        assert!(
            message.contains("AION_AUTHORING_GLEAM_PATH"),
            "validation message must name the environment override: {message}"
        );
    }

    /// CLI overrides commission the authoring loop after file/env merge.
    #[test]
    fn cli_overrides_set_authoring_paths() -> Result<(), Box<dyn std::error::Error>> {
        let mut config = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
            ",
        )?;
        let cli = CliOverrides {
            gleam_path: Some(std::path::PathBuf::from("/opt/gleam")),
            authoring_project_root: Some(std::path::PathBuf::from("/opt/project")),
            ..CliOverrides::default()
        };

        config.apply_cli_overrides(&cli);
        config.validate()?;

        assert_eq!(
            config.authoring.gleam_path.as_deref(),
            Some(std::path::Path::new("/opt/gleam"))
        );
        assert_eq!(
            config.authoring.project_root.as_deref(),
            Some(std::path::Path::new("/opt/project"))
        );
        Ok(())
    }

    #[test]
    fn invalid_values_name_problematic_field() {
        let result = ServerConfig::from_slice(
            br"
                [runtime]
                scheduler_threads = 0
            ",
        );

        let message = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        assert!(message.contains("runtime.scheduler_threads"));
    }

    #[test]
    fn cli_overrides_win_over_loaded_values() -> Result<(), Box<dyn std::error::Error>> {
        let mut config = ServerConfig::from_slice(
            br#"
                [store]
                backend = "libsql"
                url = "file.db"

                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
            "#,
        )?;
        let cli = CliOverrides {
            store_url: Some("cli.db".to_owned()),
            scheduler_threads: Some(3),
            ..CliOverrides::default()
        };

        config.apply_cli_overrides(&cli);
        config.validate()?;

        assert_eq!(config.store.url.as_deref(), Some("cli.db"));
        assert_eq!(config.runtime.scheduler_threads, 3);
        Ok(())
    }

    #[test]
    fn default_config_defaults() -> Result<(), Box<dyn std::error::Error>> {
        let mut config = ServerConfig::default();

        assert_eq!(config.store.backend, StoreBackend::Memory);
        assert_eq!(config.store.url, None);
        assert_eq!(config.server.grpc_address.to_string(), "127.0.0.1:50051");
        assert_eq!(config.server.listen_address.to_string(), "127.0.0.1:8080");
        assert_eq!(config.namespaces.default, "default");
        assert!(!config.auth.enabled);
        assert!(config.metrics.enabled);
        // event_broadcast_capacity and query_timeout_ms are the deliberately
        // defaultless values: defaults validate only once the operator
        // supplies them.
        assert_eq!(config.websocket.event_broadcast_capacity, None);
        assert_eq!(config.runtime.query_timeout_ms, None);
        config.websocket.event_broadcast_capacity = Some(64);
        config.runtime.query_timeout_ms = Some(10_000);
        config.validate()?;
        Ok(())
    }

    #[test]
    fn outbox_is_disabled_by_default_and_needs_no_knobs() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut config = ServerConfig::default();
        config.websocket.event_broadcast_capacity = Some(64);
        config.runtime.query_timeout_ms = Some(10_000);

        // The dispatcher is dark by default and its operational knobs are all
        // absent — yet validation passes, because a disabled dispatcher never
        // reads them (no assumed defaults behind the gate).
        assert!(!config.outbox.enabled);
        assert_eq!(config.outbox.poll_interval_ms, None);
        assert_eq!(config.outbox.batch_size, None);
        assert_eq!(config.outbox.max_attempts, None);
        assert_eq!(config.outbox.backoff_base_ms, None);
        assert_eq!(config.outbox.backoff_multiplier, None);
        assert_eq!(config.outbox.backoff_max_ms, None);
        config.validate()?;
        Ok(())
    }

    fn outbox_enabled_base() -> ServerConfig {
        let mut config = ServerConfig::default();
        config.websocket.event_broadcast_capacity = Some(64);
        config.runtime.query_timeout_ms = Some(10_000);
        config.outbox.enabled = true;
        config.outbox.poll_interval_ms = Some(250);
        config.outbox.batch_size = Some(64);
        config.outbox.max_attempts = Some(5);
        config.outbox.backoff_base_ms = Some(100);
        config.outbox.backoff_multiplier = Some(2);
        config.outbox.backoff_max_ms = Some(30_000);
        config
    }

    #[test]
    fn outbox_enabled_with_all_knobs_validates() -> Result<(), Box<dyn std::error::Error>> {
        outbox_enabled_base().validate()?;
        Ok(())
    }

    #[test]
    fn outbox_enabled_without_poll_interval_is_rejected() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut config = outbox_enabled_base();
        config.outbox.poll_interval_ms = None;
        let error = config
            .validate()
            .err()
            .ok_or("enabled outbox without poll interval must fail")?;
        assert!(
            error.to_string().contains("outbox.poll_interval_ms"),
            "error must name the missing key: {error}"
        );
        Ok(())
    }

    #[test]
    fn outbox_enabled_without_max_attempts_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let mut config = outbox_enabled_base();
        config.outbox.max_attempts = None;
        let error = config
            .validate()
            .err()
            .ok_or("enabled outbox without max attempts must fail")?;
        assert!(
            error.to_string().contains("outbox.max_attempts"),
            "error must name the missing key: {error}"
        );
        Ok(())
    }

    #[test]
    fn outbox_backoff_max_below_base_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let mut config = outbox_enabled_base();
        config.outbox.backoff_base_ms = Some(1_000);
        config.outbox.backoff_max_ms = Some(500);
        let error = config
            .validate()
            .err()
            .ok_or("backoff_max below backoff_base must fail")?;
        assert!(
            error.to_string().contains("outbox.backoff_max_ms"),
            "error must name the offending key: {error}"
        );
        Ok(())
    }

    #[test]
    fn package_discovery_is_sorted() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        std::fs::write(temp_dir.path().join("zeta.aion"), b"package")?;
        std::fs::write(temp_dir.path().join("alpha.aion"), b"package")?;
        std::fs::write(temp_dir.path().join("ignored.txt"), b"package")?;
        std::fs::create_dir(temp_dir.path().join("nested"))?;
        std::fs::write(
            temp_dir.path().join("nested").join("nested.aion"),
            b"package",
        )?;

        let packages = discover_workflow_packages(temp_dir.path())?;

        assert_eq!(
            packages,
            vec![
                temp_dir.path().join("alpha.aion"),
                temp_dir.path().join("zeta.aion"),
            ]
        );
        Ok(())
    }

    #[test]
    fn workflow_package_merge_is_additive_and_deduplicated() {
        let mut packages = vec!["config.aion".into(), "shared.aion".into()];
        let discovered = vec!["auto.aion".into(), "shared.aion".into()];
        let cli = vec!["cli.aion".into(), "auto.aion".into()];

        merge_workflow_packages(&mut packages, discovered, &cli);

        assert_eq!(
            packages,
            vec![
                std::path::PathBuf::from("config.aion"),
                std::path::PathBuf::from("shared.aion"),
                std::path::PathBuf::from("auto.aion"),
                std::path::PathBuf::from("cli.aion"),
            ]
        );
    }

    #[test]
    fn package_merge_deduplicates_canonical_files() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let package = temp_dir.path().join("hello.aion");
        std::fs::write(&package, b"package")?;
        let mut packages = vec![package.clone()];
        let discovered = vec![temp_dir.path().join(".").join("hello.aion")];

        merge_workflow_packages(&mut packages, discovered, &[]);

        assert_eq!(packages, vec![package]);
        Ok(())
    }

    #[test]
    fn zero_config_cli_workflow_package_uses_in_memory_defaults()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;

        let cli = CliOverrides {
            workflow_packages: vec!["hello-world.aion".into()],
            ..CliOverrides::default()
        };
        let mut config = ServerConfig::default();
        // Even zero-config development runs must size event streaming and the
        // query reply deadline explicitly (config keys or the
        // AION_WEBSOCKET_EVENT_BROADCAST_CAPACITY /
        // AION_RUNTIME_QUERY_TIMEOUT_MS environment overrides).
        config.websocket.event_broadcast_capacity = Some(64);
        config.runtime.query_timeout_ms = Some(10_000);
        config.load_discovered_workflow_packages(&cli, temp_dir.path())?;

        config.validate()?;

        assert_eq!(config.store.backend, StoreBackend::Memory);
        assert_eq!(config.store.url, None);
        assert_eq!(
            config.workflow_packages,
            vec![std::path::PathBuf::from("hello-world.aion")]
        );
        Ok(())
    }

    #[test]
    fn cli_packages_are_additive() -> Result<(), Box<dyn std::error::Error>> {
        let mut config = ServerConfig::from_slice(
            br#"
                workflow_packages = ["config.aion"]

                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
            "#,
        )?;
        let cli = CliOverrides {
            workflow_packages: vec!["cli-one.aion".into(), "cli-two.aion".into()],
            ..CliOverrides::default()
        };

        merge_workflow_packages(
            &mut config.workflow_packages,
            Vec::new(),
            &cli.workflow_packages,
        );

        assert_eq!(
            config.workflow_packages,
            vec![
                std::path::PathBuf::from("config.aion"),
                std::path::PathBuf::from("cli-one.aion"),
                std::path::PathBuf::from("cli-two.aion"),
            ]
        );
        Ok(())
    }
}
