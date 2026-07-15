//! [`ServerConfig`]: the complete merged server configuration, its load /
//! merge / validate pipeline, and the workflow-package discovery helpers.
//!
//! This is the assembly point of the config surface: [`ServerConfig`] holds one
//! field per `[section]` (defined in [`super::sections`]), and its impls carry
//! the load/merge/CLI-override/default-fill/validate pipeline plus
//! [`ServerConfig::into_parts`], which splits the durable store config from the
//! non-secret [`RuntimeConfig`] runtime view. It is re-exported from the
//! `config` module so every existing `crate::config::X` path resolves
//! identically.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;

use crate::error::ServerError;

use super::{
    AUTHORING_GLEAM_PATH_EMPTY, AUTHORING_PROJECT_ROOT_REQUIRED, AuthConfig, AuthoringConfig,
    CORS_ALLOWED_ORIGIN_INVALID, CliOverrides, ClusterConfig, DEFAULT_CLUSTER_BROADCAST_CAPACITY,
    DEFAULT_DEPLOY_MAX_ARCHIVE_BYTES, DEFAULT_DEPLOY_MAX_INFLATED_BYTES,
    DEFAULT_EVENT_BROADCAST_CAPACITY, DEFAULT_OUTBOX_BACKOFF_BASE_MS,
    DEFAULT_OUTBOX_BACKOFF_MAX_MS, DEFAULT_OUTBOX_BACKOFF_MULTIPLIER, DEFAULT_OUTBOX_BATCH_SIZE,
    DEFAULT_OUTBOX_MAX_ATTEMPTS, DEFAULT_OUTBOX_POLL_INTERVAL_MS, DEFAULT_QUERY_TIMEOUT_MS,
    DEPLOY_MAX_ARCHIVE_BYTES_REQUIRED, DEPLOY_MAX_INFLATED_BYTES_REQUIRED, DeployConfig, DevConfig,
    DrainConfig, ListenConfig, MetricsConfig, NamespaceConfig, NamespaceMode, NamespacesConfig,
    OUTBOX_BACKOFF_BASE_REQUIRED, OUTBOX_BACKOFF_MAX_REQUIRED, OUTBOX_BACKOFF_MULTIPLIER_REQUIRED,
    OUTBOX_BATCH_SIZE_REQUIRED, OUTBOX_MAX_ATTEMPTS_REQUIRED, OUTBOX_POLL_INTERVAL_REQUIRED,
    OUTBOX_RECONCILE_INTERVAL_REQUIRED, OUTBOX_RECONCILE_STALE_AFTER_REQUIRED, ObservabilityConfig,
    OpsConsoleAssetSource, OpsConsoleConfig, OutboxConfig, QUERY_TIMEOUT_REQUIRED, RuntimeConfig,
    RuntimeSection, ServerSection, StoreBackend, StoreConfig, TlsConfig, WebSocketConfig,
    WorkerConfig, config_error, env, file,
};

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
    /// Agent-observability transcript retention bounds.
    pub observability: ObservabilityConfig,
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
            observability: self.observability,
            scheduler_threads: self.runtime.scheduler_threads,
            query_timeout: self.runtime.query_timeout_ms.map(Duration::from_millis),
            default_namespace: self.namespaces.default,
            auto_create: self.namespaces.auto_create,
            max_in_flight_activities: self.namespaces.max_in_flight_activities,
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
        self.observability.validate()?;
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

#[cfg(test)]
mod tests {
    use crate::config::{AutoCreate, DEFAULT_MAX_IN_FLIGHT_ACTIVITIES, OpsConsoleAssetSource};

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
        assert_eq!(config.namespaces.auto_create, AutoCreate::Open);
        // `max_in_flight_activities` is omitted above, so it resolves to the
        // generous platform default.
        assert_eq!(
            config.namespaces.max_in_flight_activities,
            DEFAULT_MAX_IN_FLIGHT_ACTIVITIES
        );
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
        assert_eq!(config.namespaces.auto_create, AutoCreate::Closed);
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
        assert_eq!(config.namespaces.auto_create, AutoCreate::Open);
        Ok(())
    }

    #[test]
    fn namespaces_max_in_flight_activities_override_parses()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br#"
                [namespaces]
                default = "production"
                max_in_flight_activities = 32
            "#,
        )?;
        assert_eq!(config.namespaces.max_in_flight_activities, 32);
        // The override also propagates into the runtime view.
        let (_store, runtime) = config.into_parts();
        assert_eq!(runtime.max_in_flight_activities, 32);
        Ok(())
    }

    #[test]
    fn namespaces_max_in_flight_activities_defaults_when_omitted()
    -> Result<(), Box<dyn std::error::Error>> {
        // An old/minimal config that predates the field omits it entirely and
        // resolves to the generous platform default (additive, not a migration).
        let config = ServerConfig::from_slice(
            br#"
                [namespaces]
                default = "production"
            "#,
        )?;
        assert_eq!(
            config.namespaces.max_in_flight_activities,
            DEFAULT_MAX_IN_FLIGHT_ACTIVITIES
        );
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

    /// An omitted `[observability]` section resolves both retention bounds to
    /// their defaults and boots — retention is on out of the box, never a
    /// forced operator decision.
    #[test]
    fn missing_observability_section_uses_defaults() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64
            ",
        )?;
        assert_eq!(
            config.observability.max_event_bytes,
            crate::config::DEFAULT_OBSERVABILITY_MAX_EVENT_BYTES
        );
        assert_eq!(
            config.observability.max_stream_events,
            crate::config::DEFAULT_OBSERVABILITY_MAX_STREAM_EVENTS
        );
        // The bounds also ride into the runtime view the server state reads.
        let (_store, runtime) = config.into_parts();
        assert_eq!(
            runtime.observability.max_event_bytes,
            crate::config::DEFAULT_OBSERVABILITY_MAX_EVENT_BYTES
        );
        Ok(())
    }

    /// Explicit `[observability]` values parse and round-trip into the runtime
    /// view.
    #[test]
    fn observability_section_parses_and_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64

                [observability]
                max_event_bytes = 512
                max_stream_events = 3
            ",
        )?;
        assert_eq!(config.observability.max_event_bytes, 512);
        assert_eq!(config.observability.max_stream_events, 3);
        let (_store, runtime) = config.into_parts();
        assert_eq!(runtime.observability.max_event_bytes, 512);
        assert_eq!(runtime.observability.max_stream_events, 3);
        Ok(())
    }

    #[test]
    fn zero_observability_max_event_bytes_fails_startup_validation() {
        let result = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64

                [observability]
                max_event_bytes = 0
            ",
        );
        let message = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        assert!(
            message.contains("observability.max_event_bytes"),
            "validation message must name the zero-valued key: {message}"
        );
    }

    #[test]
    fn zero_observability_max_stream_events_fails_startup_validation() {
        let result = ServerConfig::from_slice(
            br"
                [runtime]
                query_timeout_ms = 10000

                [websocket]
                event_broadcast_capacity = 64
                cluster_broadcast_capacity = 64

                [observability]
                max_stream_events = 0
            ",
        );
        let message = result
            .err()
            .map_or_else(String::new, |error| error.to_string());
        assert!(
            message.contains("observability.max_stream_events"),
            "validation message must name the zero-valued key: {message}"
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
            OpsConsoleAssetSource::FileSystem { asset_path } => {
                assert_eq!(asset_path.as_os_str(), "/srv/aion/ui");
            }
            OpsConsoleAssetSource::Embedded => {
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
            OpsConsoleAssetSource::FileSystem { .. }
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
        // 64 pending #187: 4096 defeated its own lazy-materialization premise
        // (boot scan_prefix materializes all shards; commit then fsyncs per
        // shard and blows the actor timeout). See StoreConfig::default().
        assert_eq!(config.store.shard_count, 64);
        assert_eq!(config.store.url, None);
        assert_eq!(config.server.grpc_address.to_string(), "127.0.0.1:50051");
        assert_eq!(config.server.listen_address.to_string(), "127.0.0.1:8080");
        assert_eq!(config.namespaces.default, "default");
        // Minted-on-use is OPEN by default to preserve the zero-config,
        // no-pre-provision model: a namespace comes into being on first
        // worker reference.
        assert_eq!(config.namespaces.auto_create, AutoCreate::Open);
        // The cluster-wide in-flight ceiling defaults to the generous platform
        // headroom value (P2-Q1); nothing enforces it yet.
        assert_eq!(
            config.namespaces.max_in_flight_activities,
            DEFAULT_MAX_IN_FLIGHT_ACTIVITIES
        );
        assert_eq!(config.namespaces.max_in_flight_activities, 1024);
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
