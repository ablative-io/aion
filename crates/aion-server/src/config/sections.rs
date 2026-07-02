//! Typed `[section]` sub-configurations that make up [`ServerConfig`].
//!
//! Each struct/enum here maps one `[section]` (or nested value) of the server
//! TOML surface, with its `serde` attributes, `Default` impl, and the small
//! amount of per-section validation that belongs with the type
//! ([`WebSocketConfig::validate`]). They are re-exported from the `config`
//! module so every existing `crate::config::X` path resolves identically.
//!
//! [`ServerConfig`]: super::ServerConfig

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use serde::Deserialize;

use crate::error::ServerError;

use super::{
    config_error,
    defaults::{
        CLUSTER_BROADCAST_CAPACITY_REQUIRED, DEFAULT_GRPC_ADDRESS, DEFAULT_HAEMATITE_DATA_DIR,
        DEFAULT_HTTP_ADDRESS, DEFAULT_MAX_IN_FLIGHT_ACTIVITIES, EVENT_BROADCAST_CAPACITY_REQUIRED,
    },
};

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
    /// This is an IMMUTABLE virtual-shard count: nodes own shard *ranges* and
    /// routing is `BLAKE3(key) % shard_count` with no reshard path, so a
    /// single-node deployment can later grow into a cluster WITHOUT a data
    /// migration — but only up to `shard_count` nodes, and the value is fixed
    /// at create.
    ///
    /// The default was briefly 4096 on the premise that lazy shard-actor
    /// materialization (haematite >= 0.4.0) made a high count ~free. That
    /// premise fails in practice (#187): aion-server's boot restores
    /// packages/routes/namespaces via full-prefix scans, which materialize
    /// EVERY shard, and haematite 0.4.0 then fans each commit out to every
    /// materialized shard with an unconditional fsync — ~2 fsyncs x 4096 per
    /// logical commit — blowing the 5s shard-actor timeout and bricking
    /// deploy/start/timers/outbox on a fresh server. Re-raise only after
    /// haematite makes commit O(dirty shards) and the scaffold e2es pass at
    /// the new default. Set explicitly (config or `AION_STORE_SHARD_COUNT`)
    /// to override. Ignored by every other backend, and ignored when opening
    /// an existing haematite database (the on-disk shard count wins).
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
    ///
    /// [`DEFAULT_FAILOVER_POLL_INTERVAL_MS`]: super::DEFAULT_FAILOVER_POLL_INTERVAL_MS
    #[serde(default)]
    pub failover_poll_interval_ms: Option<u64>,
    /// SS-5b debounce: the number of CONSECUTIVE polls a peer must be observed
    /// disconnected before its shards are adopted, so a transient blip does not
    /// trigger a disruptive failover. Defaults to
    /// [`DEFAULT_FAILOVER_CONFIRMATIONS`] when omitted; must be at least one.
    ///
    /// [`DEFAULT_FAILOVER_CONFIRMATIONS`]: super::DEFAULT_FAILOVER_CONFIRMATIONS
    #[serde(default)]
    pub failover_confirmations: Option<u32>,
}

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
    /// Platform-wide default for a namespace's **cluster-wide** concurrent
    /// in-flight-activity ceiling, applied when a namespace record carries no
    /// explicit `max_in_flight_activities` override (Control-Plane Phase 2,
    /// P2-Q1). This is the GENEROUS platform default, NOT a low hard cap: it is
    /// a cluster-wide tenant contract (never "per-node × N"), so a tenant is
    /// promised ≈this many concurrent activities across the whole cluster.
    /// Defaults to [`DEFAULT_MAX_IN_FLIGHT_ACTIVITIES`].
    ///
    /// **Stored-only in this slice.** Nothing enforces it yet — the outbox
    /// dispatcher's keyed backpressure (P2-Q2) consults it in a later slice.
    ///
    /// [`DEFAULT_MAX_IN_FLIGHT_ACTIVITIES`]: super::DEFAULT_MAX_IN_FLIGHT_ACTIVITIES
    pub max_in_flight_activities: u32,
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

impl WebSocketConfig {
    /// Validate the three unconditionally-mounted WebSocket seams: the
    /// per-connection buffer bound, the workflow event broadcast capacity, and
    /// the WS3 cluster broadcast capacity. The two broadcast capacities are
    /// explicit-no-default with a non-zero guard, since both back a lag ->
    /// one-error-frame -> close contract that needs a defined buffer to lag
    /// against.
    pub(super) fn validate(&self) -> Result<(), ServerError> {
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
    ///
    /// [`DEFAULT_DEPLOY_MAX_ARCHIVE_BYTES`]: super::DEFAULT_DEPLOY_MAX_ARCHIVE_BYTES
    pub max_archive_bytes: Option<u64>,
    /// Inflate ceiling for uploaded archive contents, in bytes: the total
    /// decompressed size of all archive entries an upload may extract to
    /// (DEFLATE bombs inflate ~1000:1 past `max_archive_bytes`). Defaults to
    /// [`DEFAULT_DEPLOY_MAX_INFLATED_BYTES`] when omitted and `enabled = true`;
    /// must be at least `max_archive_bytes`.
    ///
    /// [`DEFAULT_DEPLOY_MAX_INFLATED_BYTES`]: super::DEFAULT_DEPLOY_MAX_INFLATED_BYTES
    pub max_inflated_bytes: Option<u64>,
}

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
    ///
    /// [`DEFAULT_OUTBOX_POLL_INTERVAL_MS`]: super::DEFAULT_OUTBOX_POLL_INTERVAL_MS
    pub poll_interval_ms: Option<u64>,
    /// Maximum number of pending rows claimed per sweep. Defaults to
    /// [`DEFAULT_OUTBOX_BATCH_SIZE`] when omitted and `enabled = true`.
    ///
    /// [`DEFAULT_OUTBOX_BATCH_SIZE`]: super::DEFAULT_OUTBOX_BATCH_SIZE
    pub batch_size: Option<u32>,
    /// Dispatch attempts before a row is dead-lettered to `failed`. Defaults to
    /// [`DEFAULT_OUTBOX_MAX_ATTEMPTS`] when omitted and `enabled = true`. Must
    /// be at least one.
    ///
    /// [`DEFAULT_OUTBOX_MAX_ATTEMPTS`]: super::DEFAULT_OUTBOX_MAX_ATTEMPTS
    pub max_attempts: Option<u32>,
    /// Base retry backoff applied to the first retry, in milliseconds. Defaults
    /// to [`DEFAULT_OUTBOX_BACKOFF_BASE_MS`] when omitted and `enabled = true`.
    /// Successive retries multiply this by `backoff_multiplier` raised to the
    /// prior-attempt count, capped at `backoff_max_ms`.
    ///
    /// [`DEFAULT_OUTBOX_BACKOFF_BASE_MS`]: super::DEFAULT_OUTBOX_BACKOFF_BASE_MS
    pub backoff_base_ms: Option<u64>,
    /// Geometric growth factor applied to the backoff per prior attempt.
    /// Defaults to [`DEFAULT_OUTBOX_BACKOFF_MULTIPLIER`] when omitted and
    /// `enabled = true`. Must be at least one so backoff never shrinks.
    ///
    /// [`DEFAULT_OUTBOX_BACKOFF_MULTIPLIER`]: super::DEFAULT_OUTBOX_BACKOFF_MULTIPLIER
    pub backoff_multiplier: Option<u32>,
    /// Upper bound on a single retry's backoff, in milliseconds. Defaults to
    /// [`DEFAULT_OUTBOX_BACKOFF_MAX_MS`] when omitted and `enabled = true`. Must
    /// be at least `backoff_base_ms`.
    ///
    /// [`DEFAULT_OUTBOX_BACKOFF_MAX_MS`]: super::DEFAULT_OUTBOX_BACKOFF_MAX_MS
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

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            listen_address: DEFAULT_HTTP_ADDRESS,
            grpc_address: DEFAULT_GRPC_ADDRESS,
            cors_allowed_origins: Vec::new(),
        }
    }
}

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
            // 64, NOT 4096 (#187): raising the default to 4096 bricked every
            // fresh server — engine boot's scan_prefix materializes every
            // shard, then each haematite 0.4.0 commit fans out one thread +
            // fsync PER MATERIALIZED SHARD (~8k fsyncs/commit), blowing the
            // 5s shard-actor timeout on deploy/start/timers/outbox. Re-raise
            // only after haematite makes commit O(dirty shards) and the
            // scaffold e2es pass at the new default (see #187 fix plan).
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
            max_in_flight_activities: DEFAULT_MAX_IN_FLIGHT_ACTIVITIES,
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
