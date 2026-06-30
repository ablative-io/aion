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
    /// Static ops-console asset bundle location.
    #[serde(alias = "dashboard")]
    pub ops_console: OpsConsoleConfig,
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
    /// Browser origins allowed to make cross-origin (CORS) requests to the
    /// public HTTP API. Empty (the default) is the SECURE default: no
    /// cross-origin request is permitted and no `CorsLayer` is installed, so a
    /// same-origin deployment behaves byte-identically to before this field
    /// existed. When set, each entry is an exact origin (scheme + host + port,
    /// e.g. `http://localhost:5173`) the browser dashboard is served from; the
    /// router then answers preflight and emits `Access-Control-Allow-Origin`
    /// for exactly those origins. There is no wildcard/allow-all default
    /// (ADR-001): cross-origin access is an explicit operator decision, and the
    /// layer never pairs `Any` with credentials.
    #[serde(default)]
    pub cors_allowed_origins: Vec<String>,
}

/// Supported event-store backend names.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum StoreBackend {
    /// In-memory store for local development.
    Memory,
    /// libSQL durable store.
    LibSql,
    /// haematite durable store (single-node, shardable).
    Haematite,
}

/// Event-store backend configuration from `[store]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StoreConfig {
    /// Selected backing store implementation.
    pub backend: StoreBackend,
    /// Backend URL/path. For libSQL this is the embedded database path; for memory it is ignored.
    pub url: Option<String>,
    /// Static distribution-shard assignment for this node (multi-shard
    /// active-active). When empty (the default) the node owns ALL shards — the
    /// single-node default, byte-identical to today. When set, the engine boot
    /// path scopes recovery and enumeration to exactly these shards. Single-shard
    /// backends (memory, libSQL) ignore the assignment; it is meaningful only for
    /// a sharded backend. No election is performed: assignment is static config.
    pub owned_shards: Vec<usize>,
    /// Filesystem data directory for the haematite backend. Required when
    /// `backend = haematite`; ignored by every other backend. The directory is
    /// opened if it already holds a haematite database, otherwise created.
    pub data_dir: Option<String>,
    /// Number of haematite shards to create on a fresh database. Defaults to 64.
    ///
    /// This is a generous, IMMUTABLE virtual-shard count: nodes own shard
    /// *ranges* and routing is `BLAKE3(key) % shard_count` with no reshard path,
    /// so a single-node deployment can later grow into a cluster WITHOUT a data
    /// migration — but only up to `shard_count` nodes, and the value is fixed at
    /// create. 64 is chosen over a Temporal-style 512 because Aion's dominant
    /// case is zero-config single-node, where boot probes every shard and writes
    /// roughly one small file per shard: an empty 512-shard DB costs ~7s boot and
    /// ~515 files, vs ~1s and ~67 files at 64, while 64 nodes is ample cluster
    /// headroom for this workload. Set it explicitly (config or
    /// `AION_STORE_SHARD_COUNT`) for larger clusters. Ignored by every other
    /// backend, and ignored when opening an existing haematite database (the
    /// on-disk shard count wins).
    pub shard_count: usize,
    /// Optional distributed-cluster membership for the haematite backend (SS-2).
    ///
    /// Absent (the default) selects the SINGLE-NODE haematite path, byte-identical
    /// to today: no endpoint is bound, no shard is elected, the store owns
    /// everything locally. Present selects the DISTRIBUTED path: the boot path
    /// binds a replication endpoint, builds a quorum membership from `members` +
    /// `peers`, and the engine boot path elects (`acquire_shard_and_serve`) this
    /// node's `owned_shards` before recovery. Ignored by every non-haematite
    /// backend.
    pub cluster: Option<ClusterConfig>,
}

/// Distributed-cluster membership for the haematite backend, from `[store.cluster]`.
///
/// This is the minimal, well-defaulted seam that turns the single-node haematite
/// store into a distributed one (SS-2). A "cluster of one" — `node_id` set,
/// `members` either empty or naming only `node_id`, and no `peers` — is a valid,
/// non-flaky configuration: election self-quorums (quorum denominator 1) and the
/// node boots through the production builder as the fenced owner of its shards.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClusterConfig {
    /// This node's globally-unique distribution name (e.g. `node-0@127.0.0.1`).
    /// Used as the local endpoint name and the local membership identity.
    pub node_id: String,
    /// The replication endpoint listen address this node binds for peer
    /// quorum/election traffic (e.g. `127.0.0.1:7000`).
    pub bind_address: SocketAddr,
    /// The FULL cluster membership by node id — the quorum DENOMINATOR. Never the
    /// reachable subset. May be empty or omit peers for a cluster of one, in which
    /// case it is treated as `[node_id]` (denominator 1). `node_id` is always
    /// counted in the denominator whether or not it appears here.
    #[serde(default)]
    pub members: Vec<String>,
    /// Dialable peers (name + address) this node connects to for replication. A
    /// cluster of one leaves this empty. Peers not in `members` do not inflate the
    /// quorum denominator.
    #[serde(default)]
    pub peers: Vec<ClusterPeer>,
    /// SS-5b automatic-failover poll interval in milliseconds: how often the
    /// cluster supervisor checks each watched peer's replication liveness.
    /// Defaults to [`DEFAULT_FAILOVER_POLL_INTERVAL_MS`] when omitted.
    #[serde(default)]
    pub failover_poll_interval_ms: Option<u64>,
    /// SS-5b debounce: the number of CONSECUTIVE polls a peer must be observed
    /// disconnected before its shards are adopted, so a transient blip does not
    /// trigger a disruptive failover. Defaults to
    /// [`DEFAULT_FAILOVER_CONFIRMATIONS`] when omitted; must be at least one.
    #[serde(default)]
    pub failover_confirmations: Option<u32>,
}

/// Default SS-5b failover poll interval (milliseconds) when `[store.cluster]`
/// does not set `failover_poll_interval_ms`.
pub const DEFAULT_FAILOVER_POLL_INTERVAL_MS: u64 = 500;

/// Default SS-5b debounce count when `[store.cluster]` does not set
/// `failover_confirmations`.
pub const DEFAULT_FAILOVER_CONFIRMATIONS: u32 = 3;

/// One dialable cluster peer: its distribution name and replication address.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClusterPeer {
    /// The peer's globally-unique distribution name (matches its `node_id`).
    pub name: String,
    /// The peer's replication endpoint address to dial.
    pub address: SocketAddr,
    /// The peer's gRPC client-API address, for request forwarding (R-2/R-3).
    /// This is DISTINCT from `address` (the replication/quorum endpoint): a
    /// forwarded client `signal`/`query`/`cancel` is dialed here, not on the
    /// replication port. Absent (the default) means the peer is not
    /// forwardable — its shards still resolve to a remote owner, but routing
    /// falls back to returning the typed `NotOwner` instead of forwarding (R-3).
    #[serde(default)]
    pub grpc_address: Option<SocketAddr>,
    /// The distribution shards this peer owns. Empty (the default) means the
    /// operator did not declare the peer's shards, so the SS-5b cluster
    /// supervisor cannot adopt them automatically when the peer dies — automatic
    /// failover for a peer requires its `owned_shards` to be declared here so the
    /// survivor knows exactly which shards to elect + resume. Declaring them does
    /// not change replication or quorum; it only tells the supervisor what to
    /// adopt on this peer's death.
    #[serde(default)]
    pub owned_shards: Vec<usize>,
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
    /// Minted-on-use policy: whether referencing an unseen namespace at worker
    /// registration durably mints it ([`AutoCreate::Open`], the zero-config
    /// default) or is rejected ([`AutoCreate::Closed`]).
    pub auto_create: AutoCreate,
}

/// Minted-on-use namespace policy (Control-Plane Phase 1).
///
/// Governs what happens when a worker registers for a namespace that has no
/// durable registry record. Defaults to [`AutoCreate::Open`] to preserve the
/// zero-ceremony, no-pre-provision model: a namespace comes into being on first
/// reference.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoCreate {
    /// A worker registering for an unseen namespace durably mints it (an
    /// idempotent upsert through the registry). Zero-config default.
    #[default]
    Open,
    /// A worker registering for a namespace with no durable registry record is
    /// rejected; the namespace is never created at the registration hook. The
    /// escape hatch for locked-down deployments is an explicit create
    /// (`POST /namespaces`).
    Closed,
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

/// Static ops-console asset configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpsConsoleConfig {
    /// Operator-selected bundle source.
    pub source: OpsConsoleAssetSource,
}

/// Static ops-console bundle source.
#[derive(Clone, Debug, Deserialize)]
pub enum OpsConsoleAssetSource {
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
    /// Capacity of the deployment-global cluster topology/ownership broadcast
    /// channel that backs the WS3 `cluster` subscription on `/events/stream`.
    /// REQUIRED with the same non-zero startup guard as
    /// [`Self::event_broadcast_capacity`]: the cluster channel uses the same
    /// lag -> one-error-frame -> close contract, which has no defined buffer to
    /// lag against unless a capacity is configured. Cluster events are low-rate
    /// (peer/shard/worker topology deltas), so this is typically far smaller
    /// than the workflow event capacity.
    pub cluster_broadcast_capacity: Option<usize>,
}

/// Default `event_broadcast_capacity` applied when omitted, so a minimal/empty
/// config boots without forcing the operator to size a tuning knob. Sized for
/// global event volume across namespaces; override for high-throughput fleets.
pub const DEFAULT_EVENT_BROADCAST_CAPACITY: usize = 256;

/// Default `cluster_broadcast_capacity` applied when omitted. Cluster topology
/// events are low-rate, so a small lag buffer is ample.
pub const DEFAULT_CLUSTER_BROADCAST_CAPACITY: usize = 64;

/// Operator-facing message for an explicitly zero `event_broadcast_capacity`
/// (omitting the key uses [`DEFAULT_EVENT_BROADCAST_CAPACITY`]; an explicit zero
/// is a genuine misconfiguration — a zero-capacity channel streams nothing).
pub(crate) const EVENT_BROADCAST_CAPACITY_REQUIRED: &str = "websocket.event_broadcast_capacity must be a positive integer when set (the server always mounts /events/stream, so a zero-capacity channel would stream nothing); omit websocket.event_broadcast_capacity (or AION_WEBSOCKET_EVENT_BROADCAST_CAPACITY) to use the default, or set it to a positive integer";

/// Operator-facing message for an explicitly zero `cluster_broadcast_capacity`
/// (omitting the key uses [`DEFAULT_CLUSTER_BROADCAST_CAPACITY`]).
pub(crate) const CLUSTER_BROADCAST_CAPACITY_REQUIRED: &str = "websocket.cluster_broadcast_capacity must be a positive integer when set (the server always mounts the WS3 cluster subscription on /events/stream); omit websocket.cluster_broadcast_capacity (or AION_WEBSOCKET_CLUSTER_BROADCAST_CAPACITY) to use the default, or set it to a positive integer";

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
    /// Upload-size ceiling for `.aion` archives, in bytes. Defaults to
    /// [`DEFAULT_DEPLOY_MAX_ARCHIVE_BYTES`] when omitted and `enabled = true`,
    /// so turning deploy on does not force sizing a security ceiling; the
    /// operator overrides it for their packages.
    pub max_archive_bytes: Option<u64>,
    /// Inflate ceiling for uploaded archive contents, in bytes: the total
    /// decompressed size of all archive entries an upload may extract to
    /// (DEFLATE bombs inflate ~1000:1 past `max_archive_bytes`). Defaults to
    /// [`DEFAULT_DEPLOY_MAX_INFLATED_BYTES`] when omitted and `enabled = true`;
    /// must be at least `max_archive_bytes`.
    pub max_inflated_bytes: Option<u64>,
}

/// Default `deploy.max_archive_bytes` applied when omitted and deploy is
/// enabled. A conservative 64 MiB upload ceiling: large enough for real
/// workflow packages, small enough to bound a single upload. Overridable.
pub const DEFAULT_DEPLOY_MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;

/// Default `deploy.max_inflated_bytes` applied when omitted and deploy is
/// enabled. A conservative 256 MiB decompressed-contents ceiling (4x the
/// archive ceiling) that still hard-caps DEFLATE-bomb inflation. Overridable.
pub const DEFAULT_DEPLOY_MAX_INFLATED_BYTES: u64 = 256 * 1024 * 1024;

/// Operator-facing message for an explicitly zero `deploy.max_archive_bytes`
/// (omitting the key uses [`DEFAULT_DEPLOY_MAX_ARCHIVE_BYTES`]; an explicit zero
/// is a genuine misconfiguration — a zero ceiling refuses every upload).
pub(crate) const DEPLOY_MAX_ARCHIVE_BYTES_REQUIRED: &str = "deploy.max_archive_bytes must be a positive number of bytes when set (a zero archive ceiling would refuse every upload); omit deploy.max_archive_bytes (or AION_DEPLOY_MAX_ARCHIVE_BYTES) to use the conservative default, or set it to a positive number of bytes sized for the deployment's packages";

/// Operator-facing message for an explicitly zero `deploy.max_inflated_bytes`
/// (omitting the key uses [`DEFAULT_DEPLOY_MAX_INFLATED_BYTES`]; an explicit
/// zero is a genuine misconfiguration — a compressed upload under
/// `deploy.max_archive_bytes` can inflate ~1000:1).
pub(crate) const DEPLOY_MAX_INFLATED_BYTES_REQUIRED: &str = "deploy.max_inflated_bytes must be a positive number of bytes when set (a compressed upload under deploy.max_archive_bytes can inflate ~1000:1, so a zero inflate ceiling is incoherent); omit deploy.max_inflated_bytes (or AION_DEPLOY_MAX_INFLATED_BYTES) to use the conservative default, or set it to a positive number of bytes no smaller than deploy.max_archive_bytes";

/// Default `query_timeout_ms` applied when omitted, so a minimal/empty config
/// boots with a sane workflow-query reply deadline instead of failing startup.
pub const DEFAULT_QUERY_TIMEOUT_MS: u64 = 10_000;

/// Operator-facing message for an explicitly zero `query_timeout_ms` (omitting
/// the key uses [`DEFAULT_QUERY_TIMEOUT_MS`]; an explicit zero is a genuine
/// misconfiguration — a zero deadline would fail every query immediately).
pub(crate) const QUERY_TIMEOUT_REQUIRED: &str = "runtime.query_timeout_ms must be a positive integer when set (the server always mounts /workflows/query, so a zero deadline would fail every query immediately); omit runtime.query_timeout_ms (or AION_RUNTIME_QUERY_TIMEOUT_MS) to use the default, or set it to a positive number of milliseconds";

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
/// commissions the dispatcher; its operational knobs below — poll interval,
/// claim batch size, retry budget, and the backoff curve — are pure tuning, so
/// each resolves to a sane default when omitted rather than forcing the
/// operator to hand-author tuning values just to turn the feature on. An
/// explicitly set value (including a misconfigured `0`) is still validated.
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
    /// Interval between successive claim sweeps, in milliseconds. Defaults to
    /// [`DEFAULT_OUTBOX_POLL_INTERVAL_MS`] when omitted and `enabled = true`;
    /// override to size the poll cadence for fan-out volume and latency budget.
    pub poll_interval_ms: Option<u64>,
    /// Maximum number of pending rows claimed per sweep. Defaults to
    /// [`DEFAULT_OUTBOX_BATCH_SIZE`] when omitted and `enabled = true`.
    pub batch_size: Option<u32>,
    /// Dispatch attempts before a row is dead-lettered to `failed`. Defaults to
    /// [`DEFAULT_OUTBOX_MAX_ATTEMPTS`] when omitted and `enabled = true`. Must
    /// be at least one.
    pub max_attempts: Option<u32>,
    /// Base retry backoff applied to the first retry, in milliseconds. Defaults
    /// to [`DEFAULT_OUTBOX_BACKOFF_BASE_MS`] when omitted and `enabled = true`.
    /// Successive retries multiply this by `backoff_multiplier` raised to the
    /// prior-attempt count, capped at `backoff_max_ms`.
    pub backoff_base_ms: Option<u64>,
    /// Geometric growth factor applied to the backoff per prior attempt.
    /// Defaults to [`DEFAULT_OUTBOX_BACKOFF_MULTIPLIER`] when omitted and
    /// `enabled = true`. Must be at least one so backoff never shrinks.
    pub backoff_multiplier: Option<u32>,
    /// Upper bound on a single retry's backoff, in milliseconds. Defaults to
    /// [`DEFAULT_OUTBOX_BACKOFF_MAX_MS`] when omitted and `enabled = true`. Must
    /// be at least `backoff_base_ms`.
    pub backoff_max_ms: Option<u64>,
    /// Interval between live stale-claim reconciliation sweeps, in milliseconds. When both
    /// reconciliation knobs are absent the live sweep remains dark; setting either knob opts into
    /// reconciliation and requires both values to be positive.
    pub reconcile_interval_ms: Option<u64>,
    /// Age after which a durable `claimed` outbox row is considered stranded, in milliseconds. The
    /// reconciler re-arms only rows with `claimed_at` older than this threshold, preserving their
    /// attempt count.
    pub reconcile_stale_after_ms: Option<u64>,
    /// Wire transport the dispatcher uses to place a claimed row with a worker.
    /// Defaults to [`OutboxTransport::Liminal`] whenever the `liminal-transport`
    /// Cargo feature is compiled (the default ablative-stack build), so an
    /// outbox-enabled server uses the liminal cross-node transport out of the box;
    /// a slim build without that feature defaults to [`OutboxTransport::Grpc`].
    /// Selecting `liminal` in a build without the feature is a configuration error
    /// surfaced at spawn. The transport only matters when `outbox.enabled = true`.
    pub transport: OutboxTransport,
    /// Address (`host:port`) the aion-server LISTENS on for inbound liminal
    /// worker connections, used only when `transport = liminal`. REQUIRED in that
    /// mode; ignored otherwise.
    ///
    /// The aion-server HOSTS the liminal listener: a remote `LiminalActivityWorker`
    /// connects IN to this address and self-registers in-band, so the server's
    /// [`ConnectionSupervisor`](liminal_server::server::connection::ConnectionSupervisor)
    /// owns the worker's connection and can push a dispatch out on it
    /// (`push_to_connection`). This replaces the superseded 13-0 spike's
    /// client-connect address: the dispatcher no longer *connects out* to publish
    /// to a channel — it pushes to a connected worker the server already owns.
    ///
    /// The dispatch *channel* is not configured here: it is derived per-row from
    /// each row's durable `(namespace, task_queue)` via `dispatch_channel_name`
    /// (NSTQ-5), so one listener fans different worker pools out by selection.
    pub liminal_listen_address: Option<String>,
}

/// Wire transport selected for outbox dispatch.
///
/// `liminal` (the default whenever the `liminal-transport` feature is compiled —
/// which it is in the default ablative-stack build) routes the dispatch over the
/// liminal cross-node bus. `grpc` keeps the connected-worker registry path. A
/// slim build compiled WITHOUT `liminal-transport` falls back to `grpc` as the
/// default so the default is always constructible under the active feature set.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutboxTransport {
    /// Dispatch over the in-process connected-worker gRPC registry. The default
    /// in a slim build compiled WITHOUT `liminal-transport`, which cannot
    /// construct the liminal path.
    // The default variant is feature-selected: gRPC when the liminal transport is
    // absent (it is the only constructible path), liminal otherwise.
    #[cfg_attr(not(feature = "liminal-transport"), default)]
    Grpc,
    /// Dispatch over the liminal cross-node bus (requires `liminal-transport`).
    /// The out-of-box default whenever `liminal-transport` is compiled (the
    /// default ablative-stack feature set), so an outbox-enabled server uses the
    /// ablative messaging transport without extra configuration.
    #[cfg_attr(feature = "liminal-transport", default)]
    Liminal,
}

/// Default `outbox.poll_interval_ms` applied when omitted and the dispatcher is
/// enabled. A tight 20ms claim cadence keeps fan-out latency low; raise it for
/// lower-rate, larger-batch sweeps.
pub const DEFAULT_OUTBOX_POLL_INTERVAL_MS: u64 = 20;

/// Default `outbox.batch_size` applied when omitted and the dispatcher is
/// enabled: rows claimed per sweep.
pub const DEFAULT_OUTBOX_BATCH_SIZE: u32 = 16;

/// Default `outbox.max_attempts` applied when omitted and the dispatcher is
/// enabled: dispatch attempts before a row is dead-lettered to `failed`.
pub const DEFAULT_OUTBOX_MAX_ATTEMPTS: u32 = 5;

/// Default `outbox.backoff_base_ms` applied when omitted and the dispatcher is
/// enabled: the first retry's backoff, in milliseconds.
pub const DEFAULT_OUTBOX_BACKOFF_BASE_MS: u64 = 50;

/// Default `outbox.backoff_multiplier` applied when omitted and the dispatcher
/// is enabled: geometric growth factor per prior attempt (must be >= 1).
pub const DEFAULT_OUTBOX_BACKOFF_MULTIPLIER: u32 = 2;

/// Default `outbox.backoff_max_ms` applied when omitted and the dispatcher is
/// enabled: the per-retry backoff ceiling, in milliseconds.
pub const DEFAULT_OUTBOX_BACKOFF_MAX_MS: u64 = 1_000;

/// Operator-facing message for an explicitly zero `outbox.poll_interval_ms`
/// (omitting the key uses [`DEFAULT_OUTBOX_POLL_INTERVAL_MS`]).
pub(crate) const OUTBOX_POLL_INTERVAL_REQUIRED: &str = "outbox.poll_interval_ms must be a positive number of milliseconds when set (a zero claim cadence is invalid); omit outbox.poll_interval_ms (or AION_OUTBOX_POLL_INTERVAL_MS) to use the default, or set it to a positive number of milliseconds sized for fan-out volume and latency";

/// Operator-facing message for an explicitly zero `outbox.batch_size`
/// (omitting the key uses [`DEFAULT_OUTBOX_BATCH_SIZE`]).
pub(crate) const OUTBOX_BATCH_SIZE_REQUIRED: &str = "outbox.batch_size must be a positive integer when set (a zero per-sweep claim ceiling claims nothing); omit outbox.batch_size (or AION_OUTBOX_BATCH_SIZE) to use the default, or set it to a positive integer";

/// Operator-facing message for an explicitly zero `outbox.max_attempts`
/// (omitting the key uses [`DEFAULT_OUTBOX_MAX_ATTEMPTS`]).
pub(crate) const OUTBOX_MAX_ATTEMPTS_REQUIRED: &str = "outbox.max_attempts must be a positive integer when set (a zero retry budget dead-letters before the first attempt); omit outbox.max_attempts (or AION_OUTBOX_MAX_ATTEMPTS) to use the default, or set it to a positive integer";

/// Operator-facing message for an explicitly zero `outbox.backoff_base_ms`
/// (omitting the key uses [`DEFAULT_OUTBOX_BACKOFF_BASE_MS`]).
pub(crate) const OUTBOX_BACKOFF_BASE_REQUIRED: &str = "outbox.backoff_base_ms must be a positive number of milliseconds when set (a zero first-retry backoff is invalid); omit outbox.backoff_base_ms (or AION_OUTBOX_BACKOFF_BASE_MS) to use the default, or set it to a positive number of milliseconds";

/// Operator-facing message for an explicitly zero `outbox.backoff_multiplier`
/// (omitting the key uses [`DEFAULT_OUTBOX_BACKOFF_MULTIPLIER`]).
pub(crate) const OUTBOX_BACKOFF_MULTIPLIER_REQUIRED: &str = "outbox.backoff_multiplier must be at least one when set so backoff never shrinks (a zero multiplier collapses the backoff curve); omit outbox.backoff_multiplier (or AION_OUTBOX_BACKOFF_MULTIPLIER) to use the default, or set it to a positive integer";

/// Operator-facing message for an explicitly undersized `outbox.backoff_max_ms`
/// (omitting the key uses [`DEFAULT_OUTBOX_BACKOFF_MAX_MS`]; an explicit value
/// must be at least `outbox.backoff_base_ms`).
pub(crate) const OUTBOX_BACKOFF_MAX_REQUIRED: &str = "outbox.backoff_max_ms must be at least outbox.backoff_base_ms when set (a ceiling below the base would cap the very first retry below its own backoff); omit outbox.backoff_max_ms (or AION_OUTBOX_BACKOFF_MAX_MS) to use the default, or set it to a positive number of milliseconds no smaller than outbox.backoff_base_ms";

/// Operator-facing message for an absent or zero `outbox.reconcile_interval_ms`.
pub(crate) const OUTBOX_RECONCILE_INTERVAL_REQUIRED: &str = "outbox.reconcile_interval_ms is required and has no default when live outbox reconciliation is enabled: set both outbox.reconcile_interval_ms and outbox.reconcile_stale_after_ms (or AION_OUTBOX_RECONCILE_INTERVAL_MS / AION_OUTBOX_RECONCILE_STALE_AFTER_MS) to positive millisecond values, or omit both to leave reconciliation disabled";

/// Operator-facing message for an absent or zero `outbox.reconcile_stale_after_ms`.
pub(crate) const OUTBOX_RECONCILE_STALE_AFTER_REQUIRED: &str = "outbox.reconcile_stale_after_ms is required and has no default when live outbox reconciliation is enabled: set both outbox.reconcile_interval_ms and outbox.reconcile_stale_after_ms (or AION_OUTBOX_RECONCILE_INTERVAL_MS / AION_OUTBOX_RECONCILE_STALE_AFTER_MS) to positive millisecond values, or omit both to leave reconciliation disabled";

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
        config.fill_operational_defaults();
        config.validate()?;
        Ok(config)
    }

    /// Fill operational tuning knobs that have a sane default when omitted, so a
    /// minimal or empty config boots without forcing the operator to hand-author
    /// values that are pure tuning. Uses `get_or_insert`, so an explicitly set
    /// value (including a misconfigured `0`, which [`Self::validate`] still
    /// rejects) is left untouched; only an absent (`None`) field is defaulted.
    fn fill_operational_defaults(&mut self) {
        self.runtime
            .query_timeout_ms
            .get_or_insert(DEFAULT_QUERY_TIMEOUT_MS);
        self.websocket
            .event_broadcast_capacity
            .get_or_insert(DEFAULT_EVENT_BROADCAST_CAPACITY);
        self.websocket
            .cluster_broadcast_capacity
            .get_or_insert(DEFAULT_CLUSTER_BROADCAST_CAPACITY);
        self.fill_outbox_defaults();
        self.fill_deploy_defaults();
    }

    /// Fill the durable-outbox tuning knobs with sane defaults when the
    /// dispatcher is enabled but a knob was omitted, so turning the feature on
    /// does not force hand-authoring pure tuning. Inert while `outbox.enabled`
    /// is false (the knobs are never read behind the gate). The reconciliation
    /// pair is intentionally NOT defaulted: when both are absent reconciliation
    /// stays dark, so forcing a default would silently commission a sweep.
    /// `get_or_insert` leaves any explicit value (including a misconfigured `0`,
    /// which [`Self::validate_outbox`] still rejects) untouched.
    fn fill_outbox_defaults(&mut self) {
        if !self.outbox.enabled {
            return;
        }
        self.outbox
            .poll_interval_ms
            .get_or_insert(DEFAULT_OUTBOX_POLL_INTERVAL_MS);
        self.outbox
            .batch_size
            .get_or_insert(DEFAULT_OUTBOX_BATCH_SIZE);
        self.outbox
            .max_attempts
            .get_or_insert(DEFAULT_OUTBOX_MAX_ATTEMPTS);
        self.outbox
            .backoff_base_ms
            .get_or_insert(DEFAULT_OUTBOX_BACKOFF_BASE_MS);
        self.outbox
            .backoff_multiplier
            .get_or_insert(DEFAULT_OUTBOX_BACKOFF_MULTIPLIER);
        self.outbox
            .backoff_max_ms
            .get_or_insert(DEFAULT_OUTBOX_BACKOFF_MAX_MS);
    }

    /// Fill the deploy decompression-bomb ceilings with conservative defaults
    /// when the deploy surface is enabled but a ceiling was omitted, so turning
    /// the feature on boots rather than refusing for want of a security knob.
    /// Inert while `deploy.enabled` is false (the ceilings are never read with
    /// the surface dark). `get_or_insert` leaves any explicit value (including a
    /// misconfigured `0` or an inflate ceiling below the archive ceiling, both
    /// still rejected by [`Self::validate`]) untouched.
    fn fill_deploy_defaults(&mut self) {
        if !self.deploy.enabled {
            return;
        }
        self.deploy
            .max_archive_bytes
            .get_or_insert(DEFAULT_DEPLOY_MAX_ARCHIVE_BYTES);
        self.deploy
            .max_inflated_bytes
            .get_or_insert(DEFAULT_DEPLOY_MAX_INFLATED_BYTES);
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
        let mut config: Self = toml::from_slice(bytes).map_err(|source| ServerError::Config {
            message: format!("invalid server config: {source}"),
        })?;
        config.fill_operational_defaults();
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
            ops_console: self.ops_console,
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
            auto_create: self.namespaces.auto_create,
            drain_timeout: Duration::from_secs(self.drain.timeout_seconds),
            metrics: self.metrics,
            owned_shards: self.store.owned_shards.clone(),
            cors_allowed_origins: self.server.cors_allowed_origins.clone(),
        };
        (self.store, runtime)
    }

    fn apply_cli_overrides(&mut self, cli: &CliOverrides) {
        if let Some(address) = cli.listen_address {
            self.server.listen_address = address;
        }
        if let Some(url) = &cli.store_url {
            self.store.url = Some(url.clone());
            // `--store-url` names an embedded libSQL database file, so it is an
            // explicit libSQL selection: coerce the implicit durable defaults
            // (memory, or the new haematite default) to libsql. An operator who
            // explicitly set `backend = "libsql"` already lands here too. The
            // haematite backend ignores `store.url`, so the only way `--store-url`
            // is meaningful is as a libSQL choice.
            if matches!(
                self.store.backend,
                StoreBackend::Memory | StoreBackend::Haematite
            ) {
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
        validate_cors_origins(&self.server.cors_allowed_origins)?;
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
        if matches!(self.store.backend, StoreBackend::Haematite) {
            if self.store.data_dir.as_deref().is_none_or(str::is_empty) {
                return config_error(
                    "store.data_dir must not be empty when store.backend is haematite",
                );
            }
            if self.store.shard_count == 0 {
                return config_error("store.shard_count must be greater than zero");
            }
            if let Some(cluster) = &self.store.cluster {
                validate_cluster(cluster)?;
            }
        } else if self.store.cluster.is_some() {
            return config_error("store.cluster is only valid when store.backend is haematite");
        }
        if let OpsConsoleAssetSource::FileSystem { asset_path } = &self.ops_console.source {
            if asset_path.as_os_str().is_empty() {
                return config_error("ops_console.source.FileSystem.asset_path must not be empty");
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
        self.websocket.validate()?;
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
        match (
            self.outbox.reconcile_interval_ms,
            self.outbox.reconcile_stale_after_ms,
        ) {
            (None, None) => {}
            (None | Some(0), _) => return config_error(OUTBOX_RECONCILE_INTERVAL_REQUIRED),
            (_, None | Some(0)) => return config_error(OUTBOX_RECONCILE_STALE_AFTER_REQUIRED),
            (Some(_), Some(_)) => {}
        }
        Ok(())
    }
}

/// Validate a `[store.cluster]` section: a non-empty node id, and every member /
/// peer name non-empty. A cluster of one (no peers, members empty or `[node_id]`)
/// is valid.
fn validate_cluster(cluster: &ClusterConfig) -> Result<(), ServerError> {
    if cluster.node_id.is_empty() {
        return config_error("store.cluster.node_id must not be empty");
    }
    if cluster.members.iter().any(String::is_empty) {
        return config_error("store.cluster.members entries must not be empty");
    }
    if cluster.peers.iter().any(|peer| peer.name.is_empty()) {
        return config_error("store.cluster.peers entries must name a non-empty node");
    }
    if matches!(cluster.failover_poll_interval_ms, Some(0)) {
        return config_error(
            "store.cluster.failover_poll_interval_ms must be greater than zero when set",
        );
    }
    if matches!(cluster.failover_confirmations, Some(0)) {
        return config_error("store.cluster.failover_confirmations must be at least one when set");
    }
    Ok(())
}

/// Operator-facing message for an empty or malformed `cors_allowed_origins`
/// entry.
pub(crate) const CORS_ALLOWED_ORIGIN_INVALID: &str = "server.cors_allowed_origins entries must each be a valid HTTP origin (scheme://host[:port], e.g. http://localhost:5173) with no path or trailing slash";

/// Validate every `[server] cors_allowed_origins` entry.
fn validate_cors_origins(origins: &[String]) -> Result<(), ServerError> {
    for origin in origins {
        validate_cors_origin(origin)?;
    }
    Ok(())
}

/// Validate one `[server] cors_allowed_origins` entry: it must be a non-empty,
/// parseable HTTP origin so the `CorsLayer` can match it against the browser's
/// `Origin` header. A malformed origin can never match a real request, so it is
/// a misconfiguration caught at startup rather than silently never matching.
fn validate_cors_origin(origin: &str) -> Result<(), ServerError> {
    if origin.is_empty() {
        return config_error(CORS_ALLOWED_ORIGIN_INVALID);
    }
    // An origin is scheme + host + optional port and carries no path: reject a
    // trailing slash or any path segment, which would never equal a browser
    // `Origin` header value.
    let scheme_split = origin.split_once("://");
    let Some((scheme, authority)) = scheme_split else {
        return config_error(CORS_ALLOWED_ORIGIN_INVALID);
    };
    if scheme.is_empty() || authority.is_empty() || authority.contains('/') {
        return config_error(CORS_ALLOWED_ORIGIN_INVALID);
    }
    // It must parse as an HTTP header value (the form the CorsLayer compares).
    if origin.parse::<axum::http::HeaderValue>().is_err() {
        return config_error(CORS_ALLOWED_ORIGIN_INVALID);
    }
    Ok(())
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
            cors_allowed_origins: Vec::new(),
        }
    }
}

/// Default haematite data directory for the unconfigured durable backend.
///
/// An empty `[store]` section (or no config file at all) now selects the ablative
/// stack's haematite event store rooted here, so a stock server is durable out of
/// the box. `validate()` requires a non-empty `data_dir` when the backend is
/// haematite, so the default must supply one or an otherwise-empty config would
/// fail validation. Operators override it with `store.data_dir` /
/// `AION_STORE_DATA_DIR`, or opt out with `backend = "memory"` / `"libsql"`.
pub const DEFAULT_HAEMATITE_DATA_DIR: &str = "aion-data";

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            // The ablative stack is the out-of-box durable default: an empty
            // config selects the haematite backend rooted at
            // `DEFAULT_HAEMATITE_DATA_DIR`. `memory` (ephemeral) and `libsql`
            // (lightweight, opt-in feature) remain explicit operator choices.
            backend: StoreBackend::Haematite,
            url: None,
            owned_shards: Vec::new(),
            data_dir: Some(DEFAULT_HAEMATITE_DATA_DIR.to_owned()),
            shard_count: 64,
            cluster: None,
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
            auto_create: AutoCreate::default(),
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

impl Default for OpsConsoleConfig {
    fn default() -> Self {
        Self {
            source: OpsConsoleAssetSource::Embedded,
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

impl WebSocketConfig {
    /// Validate the three unconditionally-mounted WebSocket seams: the
    /// per-connection buffer bound, the workflow event broadcast capacity, and
    /// the WS3 cluster broadcast capacity. The two broadcast capacities are
    /// explicit-no-default with a non-zero guard, since both back a lag ->
    /// one-error-frame -> close contract that needs a defined buffer to lag
    /// against.
    fn validate(&self) -> Result<(), ServerError> {
        if self.outbound_buffer_bound == 0 {
            return config_error("websocket.outbound_buffer_bound must be greater than zero");
        }
        match self.event_broadcast_capacity {
            None | Some(0) => return config_error(EVENT_BROADCAST_CAPACITY_REQUIRED),
            Some(_) => {}
        }
        match self.cluster_broadcast_capacity {
            None | Some(0) => return config_error(CLUSTER_BROADCAST_CAPACITY_REQUIRED),
            Some(_) => {}
        }
        Ok(())
    }
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            outbound_buffer_bound: 32,
            // Deliberately absent: validation fails loudly until the operator
            // sizes the engine-global broadcast channel for the deployment.
            event_broadcast_capacity: None,
            // Deliberately absent for the same reason: the cluster channel has
            // no defined lag buffer until sized.
            cluster_broadcast_capacity: None,
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
        CliOverrides, DEFAULT_CLUSTER_BROADCAST_CAPACITY, DEFAULT_DEPLOY_MAX_ARCHIVE_BYTES,
        DEFAULT_DEPLOY_MAX_INFLATED_BYTES, DEFAULT_EVENT_BROADCAST_CAPACITY,
        DEFAULT_OUTBOX_BACKOFF_BASE_MS, DEFAULT_OUTBOX_BACKOFF_MAX_MS,
        DEFAULT_OUTBOX_BACKOFF_MULTIPLIER, DEFAULT_OUTBOX_BATCH_SIZE, DEFAULT_OUTBOX_MAX_ATTEMPTS,
        DEFAULT_OUTBOX_POLL_INTERVAL_MS, DEFAULT_QUERY_TIMEOUT_MS, ServerConfig, StoreBackend,
        discover_workflow_packages, merge_workflow_packages,
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
                cluster_broadcast_capacity = 1024
            "#,
        )?;

        assert_eq!(config.store.backend, StoreBackend::LibSql);
        assert_eq!(config.store.url.as_deref(), Some("aion.db"));
        assert_eq!(config.runtime.scheduler_threads, 2);
        assert_eq!(config.runtime.query_timeout_ms, Some(10_000));
        assert_eq!(config.namespaces.default, "production");
        // `auto_create` is omitted above, so it resolves to the Open default.
        assert_eq!(config.namespaces.auto_create, super::AutoCreate::Open);
        assert_eq!(config.websocket.outbound_buffer_bound, 16);
        assert_eq!(config.websocket.event_broadcast_capacity, Some(1024));
        Ok(())
    }

    #[test]
    fn namespaces_auto_create_closed_parses() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br#"
                [namespaces]
                default = "production"
                auto_create = "closed"
            "#,
        )?;
        assert_eq!(config.namespaces.default, "production");
        assert_eq!(config.namespaces.auto_create, super::AutoCreate::Closed);
        Ok(())
    }

    #[test]
    fn namespaces_auto_create_open_parses() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br#"
                [namespaces]
                auto_create = "open"
            "#,
        )?;
        assert_eq!(config.namespaces.auto_create, super::AutoCreate::Open);
        Ok(())
    }

    #[test]
    fn namespaces_auto_create_rejects_unknown_variant() {
        let result = ServerConfig::from_slice(
            br#"
                [namespaces]
                auto_create = "sometimes"
            "#,
        );
        assert!(
            result.is_err(),
            "an unknown auto_create variant must fail to parse"
        );
    }

    #[test]
    fn missing_event_broadcast_capacity_uses_default() -> Result<(), Box<dyn std::error::Error>> {
        // The server unconditionally mounts /events/stream, but the channel
        // capacity is a tuning knob: omitting it must resolve to the default and
        // boot, not fail startup.
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                cluster_broadcast_capacity = 64
            ",
        )?;
        assert_eq!(
            config.websocket.event_broadcast_capacity,
            Some(DEFAULT_EVENT_BROADCAST_CAPACITY),
            "omitted event_broadcast_capacity must resolve to the default"
        );
        Ok(())
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
    fn missing_cluster_broadcast_capacity_uses_default() -> Result<(), Box<dyn std::error::Error>> {
        // A config that sizes the workflow channel but omits the low-rate cluster
        // channel must resolve the cluster capacity to its default and boot, not
        // fail loudly.
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                scheduler_threads = 1
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
            ",
        )?;
        assert_eq!(
            config.websocket.cluster_broadcast_capacity,
            Some(DEFAULT_CLUSTER_BROADCAST_CAPACITY),
            "omitted cluster_broadcast_capacity must resolve to the default"
        );
        Ok(())
    }

    #[test]
    fn zero_cluster_broadcast_capacity_fails_startup_validation() {
        let result = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 0
            ",
        );

        let message = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        assert!(
            message.contains("websocket.cluster_broadcast_capacity"),
            "validation message must name the zero-valued cluster key: {message}"
        );
    }

    #[test]
    fn missing_query_timeout_uses_default() -> Result<(), Box<dyn std::error::Error>> {
        // The server unconditionally mounts /workflows/query, but the reply
        // deadline is a tuning knob: omitting it must resolve to the default and
        // boot, not fail startup.
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                scheduler_threads = 1

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64
            ",
        )?;
        assert_eq!(
            config.runtime.query_timeout_ms,
            Some(DEFAULT_QUERY_TIMEOUT_MS),
            "omitted query_timeout_ms must resolve to the default"
        );
        Ok(())
    }

    #[test]
    fn empty_config_boots_on_operational_defaults() -> Result<(), Box<dyn std::error::Error>> {
        // The headline zero-config contract: an empty TOML must parse, fill every
        // operational tuning knob with its default, and validate — so `aion
        // server` runs with no hand-authored file. The durable default backend
        // (haematite under its default data_dir) carries the store side.
        let config = ServerConfig::from_slice(b"")?;
        assert_eq!(config.store.backend, StoreBackend::Haematite);
        assert_eq!(
            config.runtime.query_timeout_ms,
            Some(DEFAULT_QUERY_TIMEOUT_MS)
        );
        assert_eq!(
            config.websocket.event_broadcast_capacity,
            Some(DEFAULT_EVENT_BROADCAST_CAPACITY)
        );
        assert_eq!(
            config.websocket.cluster_broadcast_capacity,
            Some(DEFAULT_CLUSTER_BROADCAST_CAPACITY)
        );
        Ok(())
    }

    #[test]
    fn zero_query_timeout_fails_startup_validation() {
        let result = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 0

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64
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
    /// the archive ceiling is a conservative security default, not a forced
    /// operator decision: enabling deploy without it must resolve the ceiling
    /// to [`DEFAULT_DEPLOY_MAX_ARCHIVE_BYTES`] and boot, not fail startup.
    #[test]
    fn deploy_enabled_defaults_max_archive_bytes() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64

                [deploy]
                enabled = true
            ",
        )?;

        assert_eq!(
            config.deploy.max_archive_bytes,
            Some(DEFAULT_DEPLOY_MAX_ARCHIVE_BYTES),
            "omitted max_archive_bytes must resolve to the conservative default"
        );
        assert_eq!(
            config.deploy.max_inflated_bytes,
            Some(DEFAULT_DEPLOY_MAX_INFLATED_BYTES),
            "omitted max_inflated_bytes must resolve to the conservative default"
        );
        Ok(())
    }

    #[test]
    fn deploy_zero_max_archive_bytes_fails_startup_validation() {
        let result = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64

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

    /// The inflate ceiling defaults independently of an explicit archive
    /// ceiling: setting only `max_archive_bytes` must resolve the inflate
    /// ceiling to [`DEFAULT_DEPLOY_MAX_INFLATED_BYTES`] (which exceeds a 16 MiB
    /// archive, so the invariant holds) and boot.
    #[test]
    fn deploy_enabled_defaults_max_inflated_bytes() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64

                [deploy]
                enabled = true
                max_archive_bytes = 16777216
            ",
        )?;

        assert_eq!(
            config.deploy.max_archive_bytes,
            Some(16_777_216),
            "explicit max_archive_bytes must be left untouched"
        );
        assert_eq!(
            config.deploy.max_inflated_bytes,
            Some(DEFAULT_DEPLOY_MAX_INFLATED_BYTES),
            "omitted max_inflated_bytes must resolve to the conservative default"
        );
        Ok(())
    }

    #[test]
    fn deploy_zero_max_inflated_bytes_fails_startup_validation() {
        let result = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64

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
                cluster_broadcast_capacity = 64

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
                cluster_broadcast_capacity = 64
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
                cluster_broadcast_capacity = 64

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

    /// With no `[server] cors_allowed_origins` the list is empty: the secure
    /// default, where no cross-origin request is permitted and no `CorsLayer`
    /// is installed.
    #[test]
    fn cors_allowed_origins_default_empty() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64
            ",
        )?;

        assert!(config.server.cors_allowed_origins.is_empty());
        let (_, runtime) = config.into_parts();
        assert!(runtime.cors_allowed_origins.is_empty());
        Ok(())
    }

    /// A configured `[server] cors_allowed_origins` list parses and round-trips
    /// into `RuntimeConfig` (the value the `CorsLayer` is built from).
    #[test]
    fn cors_allowed_origins_parse_and_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br#"
                [server]
                cors_allowed_origins = ["http://localhost:5173", "http://127.0.0.1:5173"]

                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64
            "#,
        )?;

        assert_eq!(
            config.server.cors_allowed_origins,
            vec![
                "http://localhost:5173".to_owned(),
                "http://127.0.0.1:5173".to_owned()
            ]
        );
        let (_, runtime) = config.into_parts();
        assert_eq!(
            runtime.cors_allowed_origins,
            vec![
                "http://localhost:5173".to_owned(),
                "http://127.0.0.1:5173".to_owned()
            ]
        );
        Ok(())
    }

    /// A malformed CORS origin (no scheme, or a trailing path) can never match a
    /// browser `Origin` header, so it fails startup validation rather than
    /// silently never matching.
    #[test]
    fn cors_allowed_origins_reject_malformed() {
        for bad in ["", "localhost:5173", "http://localhost:5173/"] {
            let toml = format!(
                "[server]\ncors_allowed_origins = [\"{bad}\"]\n\n[runtime]\nquery_timeout_ms = 10000\n\n[websocket]\nevent_broadcast_capacity = 64\n"
            );
            let result = ServerConfig::from_slice(toml.as_bytes());
            let message = result
                .err()
                .map_or_else(String::new, |error| error.to_string());
            assert!(
                message.contains("cors_allowed_origins"),
                "malformed origin `{bad}` must be rejected naming the key: {message}"
            );
        }
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
                cluster_broadcast_capacity = 64
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
                cluster_broadcast_capacity = 64

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
                cluster_broadcast_capacity = 64
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
                cluster_broadcast_capacity = 64

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
                cluster_broadcast_capacity = 64

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
                cluster_broadcast_capacity = 64

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
                cluster_broadcast_capacity = 64
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

    /// The config field renamed `dashboard` -> `ops_console` carries a serde
    /// alias so existing `[dashboard]` TOML still parses (non-breaking rename).
    #[test]
    fn legacy_dashboard_section_alias_still_parses() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br#"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64

                [dashboard]
                source = { FileSystem = { asset_path = "/srv/aion/ui" } }
            "#,
        )?;
        match &config.ops_console.source {
            super::OpsConsoleAssetSource::FileSystem { asset_path } => {
                assert_eq!(asset_path.as_os_str(), "/srv/aion/ui");
            }
            super::OpsConsoleAssetSource::Embedded => {
                return Err("legacy [dashboard] section must map to ops_console".into());
            }
        }
        Ok(())
    }

    /// The new `[ops_console]` section name also parses.
    #[test]
    fn ops_console_section_parses() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br#"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64

                [ops_console]
                source = { FileSystem = { asset_path = "/srv/aion/ui" } }
            "#,
        )?;
        assert!(matches!(
            config.ops_console.source,
            super::OpsConsoleAssetSource::FileSystem { .. }
        ));
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
                cluster_broadcast_capacity = 64
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

        // The ablative stack is the out-of-box durable default: an empty config
        // selects the haematite backend rooted at the default data_dir, so a stock
        // server is durable without any [store] configuration. The default MUST
        // carry data_dir or validate() would reject it (data_dir is required for
        // haematite).
        assert_eq!(config.store.backend, StoreBackend::Haematite);
        assert_eq!(config.store.data_dir.as_deref(), Some("aion-data"));
        // Generous immutable virtual-shard default: cluster-capable without
        // taxing single-node first-boot (see StoreConfig::shard_count docs).
        assert_eq!(config.store.shard_count, 64);
        assert_eq!(config.store.url, None);
        assert_eq!(config.server.grpc_address.to_string(), "127.0.0.1:50051");
        assert_eq!(config.server.listen_address.to_string(), "127.0.0.1:8080");
        assert_eq!(config.namespaces.default, "default");
        // Minted-on-use is OPEN by default to preserve the zero-config,
        // no-pre-provision model: a namespace comes into being on first
        // worker reference.
        assert_eq!(config.namespaces.auto_create, super::AutoCreate::Open);
        assert!(!config.auth.enabled);
        assert!(config.metrics.enabled);
        // event_broadcast_capacity and query_timeout_ms are the deliberately
        // defaultless values: defaults validate only once the operator
        // supplies them.
        assert_eq!(config.websocket.event_broadcast_capacity, None);
        assert_eq!(config.websocket.cluster_broadcast_capacity, None);
        assert_eq!(config.runtime.query_timeout_ms, None);
        config.websocket.event_broadcast_capacity = Some(64);
        config.websocket.cluster_broadcast_capacity = Some(64);
        config.runtime.query_timeout_ms = Some(10_000);
        config.validate()?;
        Ok(())
    }

    #[test]
    fn outbox_is_disabled_by_default_and_needs_no_knobs() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut config = ServerConfig::default();
        config.websocket.event_broadcast_capacity = Some(64);
        config.websocket.cluster_broadcast_capacity = Some(64);
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
        assert_eq!(config.outbox.reconcile_interval_ms, None);
        assert_eq!(config.outbox.reconcile_stale_after_ms, None);
        config.validate()?;
        Ok(())
    }

    fn outbox_enabled_base() -> ServerConfig {
        let mut config = ServerConfig::default();
        config.websocket.event_broadcast_capacity = Some(64);
        config.websocket.cluster_broadcast_capacity = Some(64);
        config.runtime.query_timeout_ms = Some(10_000);
        config.outbox.enabled = true;
        config.outbox.poll_interval_ms = Some(250);
        config.outbox.batch_size = Some(64);
        config.outbox.max_attempts = Some(5);
        config.outbox.backoff_base_ms = Some(100);
        config.outbox.backoff_multiplier = Some(2);
        config.outbox.backoff_max_ms = Some(30_000);
        config.outbox.reconcile_interval_ms = Some(1_000);
        config.outbox.reconcile_stale_after_ms = Some(60_000);
        config
    }

    #[test]
    fn outbox_enabled_with_all_knobs_validates() -> Result<(), Box<dyn std::error::Error>> {
        outbox_enabled_base().validate()?;
        Ok(())
    }

    #[test]
    fn outbox_enabled_defaults_poll_interval() -> Result<(), Box<dyn std::error::Error>> {
        // Enabling the dispatcher but omitting the poll cadence must resolve it
        // to the default and boot, not fail startup — the cadence is pure
        // tuning, not a forced operator decision.
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64

                [outbox]
                enabled = true
            ",
        )?;
        assert_eq!(
            config.outbox.poll_interval_ms,
            Some(DEFAULT_OUTBOX_POLL_INTERVAL_MS),
            "omitted poll_interval_ms must resolve to the default"
        );
        Ok(())
    }

    #[test]
    fn outbox_enabled_defaults_max_attempts() -> Result<(), Box<dyn std::error::Error>> {
        // Setting a tuning knob explicitly but omitting the retry budget must
        // leave the explicit knob untouched and default only the omitted one.
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64

                [outbox]
                enabled = true
                poll_interval_ms = 250
            ",
        )?;
        assert_eq!(
            config.outbox.poll_interval_ms,
            Some(250),
            "explicit poll_interval_ms must be left untouched"
        );
        assert_eq!(
            config.outbox.max_attempts,
            Some(DEFAULT_OUTBOX_MAX_ATTEMPTS),
            "omitted max_attempts must resolve to the default"
        );
        Ok(())
    }

    #[test]
    fn outbox_enabled_with_only_enabled_flag_uses_all_defaults()
    -> Result<(), Box<dyn std::error::Error>> {
        // Headline conditional-default contract: an outbox section with nothing
        // but `enabled = true` validates with every tuning knob resolved to its
        // default. The reconciliation pair stays dark (both absent), as before.
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64

                [outbox]
                enabled = true
            ",
        )?;
        assert!(config.outbox.enabled);
        assert_eq!(
            config.outbox.poll_interval_ms,
            Some(DEFAULT_OUTBOX_POLL_INTERVAL_MS)
        );
        assert_eq!(config.outbox.batch_size, Some(DEFAULT_OUTBOX_BATCH_SIZE));
        assert_eq!(
            config.outbox.max_attempts,
            Some(DEFAULT_OUTBOX_MAX_ATTEMPTS)
        );
        assert_eq!(
            config.outbox.backoff_base_ms,
            Some(DEFAULT_OUTBOX_BACKOFF_BASE_MS)
        );
        assert_eq!(
            config.outbox.backoff_multiplier,
            Some(DEFAULT_OUTBOX_BACKOFF_MULTIPLIER)
        );
        assert_eq!(
            config.outbox.backoff_max_ms,
            Some(DEFAULT_OUTBOX_BACKOFF_MAX_MS)
        );
        // Reconciliation is not force-defaulted: both knobs stay absent so the
        // live sweep remains dark.
        assert_eq!(config.outbox.reconcile_interval_ms, None);
        assert_eq!(config.outbox.reconcile_stale_after_ms, None);
        Ok(())
    }

    #[test]
    fn outbox_enabled_zero_poll_interval_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        // An explicit zero is a misconfiguration the default never masks:
        // `get_or_insert` leaves `Some(0)` untouched and validate rejects it.
        let mut config = outbox_enabled_base();
        config.outbox.poll_interval_ms = Some(0);
        let error = config
            .validate()
            .err()
            .ok_or("enabled outbox with zero poll interval must fail")?;
        assert!(
            error.to_string().contains("outbox.poll_interval_ms"),
            "error must name the zero-valued key: {error}"
        );
        Ok(())
    }

    #[test]
    fn outbox_enabled_zero_max_attempts_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let mut config = outbox_enabled_base();
        config.outbox.max_attempts = Some(0);
        let error = config
            .validate()
            .err()
            .ok_or("enabled outbox with zero max attempts must fail")?;
        assert!(
            error.to_string().contains("outbox.max_attempts"),
            "error must name the zero-valued key: {error}"
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
    fn outbox_enabled_can_leave_reconciliation_dark() -> Result<(), Box<dyn std::error::Error>> {
        let mut config = outbox_enabled_base();
        config.outbox.reconcile_interval_ms = None;
        config.outbox.reconcile_stale_after_ms = None;
        config.validate()?;
        Ok(())
    }

    #[test]
    fn outbox_reconciliation_requires_interval_when_partially_enabled()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut config = outbox_enabled_base();
        config.outbox.reconcile_interval_ms = None;
        let error = config
            .validate()
            .err()
            .ok_or("reconciliation without interval must fail")?;
        assert!(error.to_string().contains("outbox.reconcile_interval_ms"));
        Ok(())
    }

    #[test]
    fn outbox_reconciliation_requires_stale_threshold_when_partially_enabled()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut config = outbox_enabled_base();
        config.outbox.reconcile_stale_after_ms = None;
        let error = config
            .validate()
            .err()
            .ok_or("reconciliation without stale threshold must fail")?;
        assert!(
            error
                .to_string()
                .contains("outbox.reconcile_stale_after_ms")
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
        // This test exercises CLI workflow-package discovery against the ephemeral
        // in-memory store, so it opts OUT of the new durable haematite default
        // explicitly (the default would otherwise carry a haematite data_dir).
        config.store.backend = StoreBackend::Memory;
        config.store.data_dir = None;
        // Even zero-config development runs must size event streaming and the
        // query reply deadline explicitly (config keys or the
        // AION_WEBSOCKET_EVENT_BROADCAST_CAPACITY /
        // AION_RUNTIME_QUERY_TIMEOUT_MS environment overrides).
        config.websocket.event_broadcast_capacity = Some(64);
        config.websocket.cluster_broadcast_capacity = Some(64);
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
                cluster_broadcast_capacity = 64
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
