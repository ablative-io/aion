//! `WorkerConfig` endpoint, task queue, identity, concurrency, and TLS/credentials passthrough.

use std::fmt;
use std::time::Duration;

/// Opaque credentials forwarded to the worker transport layer.
///
/// The worker SDK does not interpret this value or define an authentication
/// scheme. It exists only so operators can pass transport-specific credentials
/// to the session implementation that knows how to apply them.
#[derive(Clone, PartialEq, Eq)]
pub struct TransportCredentials {
    secret: Vec<u8>,
}

impl TransportCredentials {
    /// Creates opaque transport credentials from caller-supplied bytes.
    #[must_use]
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self {
            secret: secret.into(),
        }
    }

    /// Returns the opaque credential bytes for transport-specific forwarding.
    #[must_use]
    pub fn secret(&self) -> &[u8] {
        &self.secret
    }
}

impl fmt::Debug for TransportCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportCredentials")
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Operator-supplied reconnect backoff settings.
///
/// The settings govern both session establishment and the run loop's
/// cumulative mid-run session-drop budget.
///
/// **Budget reset:** the cumulative drop budget resets to zero once an
/// established session proves healthy — it served at least one task, or it
/// stayed connected longer than `max_backoff` (measured monotonically from
/// successful registration to the moment the stream ended or dropped;
/// post-drop draining of in-flight activities never extends it). The cap is
/// the policy's own definition of the longest pause, so a session outliving
/// it is demonstrably past the flapping regime, and a served task proves
/// end-to-end health. A genuinely flapping server — no session ever serves
/// a task or outlives `max_backoff` — exhausts the budget after exactly
/// `max_attempts` drops.
///
/// **Drains and clean closes:** a server-announced drain (the wire
/// `DrainRequest` frame) is an unbudgeted drop — the worker finishes
/// in-flight work and redials after `initial_backoff`; the drain
/// classification latches for the session, so even an abrupt end after the
/// frame stays drain-class. An *unannounced* clean stream close remains a
/// budgeted retryable drop: the worker redials through the same budgeted,
/// backed-off cycle, and only a persistent unannounced clean-close loop
/// exhausts the budget (surfacing
/// [`crate::error::WorkerError::CleanCloseExhausted`]).
///
/// **Shutdown during a drop backoff:** every SDK races the backoff sleep
/// against the shutdown signal and returns promptly, and the run outcome is
/// aligned across the Rust, Python, and TypeScript workers: a pending
/// drain-class or clean-close drop ends the run cleanly, while a pending
/// error-class drop surfaces its error — a supervisor sees "this worker was
/// mid-fault" distinctly from "this worker drained cleanly".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconnectConfig {
    /// Initial reconnect backoff delay. Must be non-zero before reconnecting.
    pub initial_backoff: Duration,
    /// Maximum reconnect backoff delay cap. Must be non-zero before
    /// reconnecting. Doubles as the session-health threshold for the
    /// drop-budget reset described on this type.
    pub max_backoff: Duration,
    /// Maximum reconnect attempts before surfacing the last connection error.
    pub max_attempts: usize,
}

impl ReconnectConfig {
    /// Creates reconnect settings with every field supplied explicitly.
    #[must_use]
    pub const fn new(
        initial_backoff: Duration,
        max_backoff: Duration,
        max_attempts: usize,
    ) -> Self {
        Self {
            initial_backoff,
            max_backoff,
            max_attempts,
        }
    }
}

/// Operator-supplied worker connection and serving configuration.
///
/// Tunable fields remain caller-supplied. Namespace authorization metadata
/// defaults to `default`/`worker` so development workers can register against
/// the default namespace without an explicit auth setup.
///
/// `namespaces`, `task_queue`, and `node` are disjoint routing dimensions.
/// `namespaces` is the SET of correctness/isolation boundaries the worker is
/// authorized for — the same set carried (comma-joined) in the
/// `x-aion-namespaces` auth metadata and in the registration scope, so a worker
/// registers into exactly the namespaces it is authorized for. `task_queue` is
/// the pool/flavour selector *within* each namespace. `node` is an OPTIONAL
/// locality affinity the worker advertises (default = machine hostname).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerConfig {
    /// Set of correctness/isolation boundaries this worker registers into.
    /// Advertised both in `x-aion-namespaces` worker stream metadata (comma
    /// joined) and as the registration namespace set, so authorization and
    /// registration scope agree. Must be non-empty.
    pub namespaces: Vec<String>,
    /// Subject advertised in `x-aion-subject` worker stream metadata.
    pub subject: String,
    /// Engine worker endpoint URI.
    pub endpoint: String,
    /// Pool/flavour selector within each namespace, sent as the registration's
    /// `task_queue`. The worker-pool address is `(namespace, task_queue)`.
    pub task_queue: String,
    /// Locality affinity advertised at registration. Defaults to the machine
    /// hostname; a dispatch pinned to this node reaches this worker.
    pub node: String,
    /// Worker identity used by operators and future wire metadata.
    pub identity: String,
    /// Maximum concurrent activities this worker may serve.
    pub max_concurrency: usize,
    /// Operator-supplied reconnect settings.
    pub reconnect: ReconnectConfig,
    /// Opaque credentials for the transport implementation.
    pub transport_credentials: Option<TransportCredentials>,
}

const DEFAULT_WORKER_NAMESPACE: &str = "default";
const DEFAULT_WORKER_SUBJECT: &str = "worker";

/// Fallback node id when the machine hostname cannot be resolved. A worker must
/// never fail to start over a missing hostname, so it advertises this documented
/// default instead of panicking.
const DEFAULT_WORKER_NODE: &str = "localhost";

/// Resolve the machine hostname for use as the default node locality affinity.
///
/// Falls back to [`DEFAULT_WORKER_NODE`] (never panics) when the OS hostname is
/// unavailable or is not valid UTF-8.
#[must_use]
pub fn default_node() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|hostname| !hostname.is_empty())
        .or_else(|| {
            hostname::get()
                .ok()
                .and_then(|raw| raw.into_string().ok())
                .filter(|hostname| !hostname.is_empty())
        })
        .unwrap_or_else(|| String::from(DEFAULT_WORKER_NODE))
}

impl WorkerConfig {
    /// Starts an explicit builder. The caller must provide every required field
    /// before calling [`WorkerConfigBuilder::build`].
    #[must_use]
    pub const fn builder() -> WorkerConfigBuilder {
        WorkerConfigBuilder::new()
    }

    /// Creates a worker config with default authorization metadata.
    #[must_use]
    pub fn new(
        endpoint: impl Into<String>,
        task_queue: impl Into<String>,
        identity: impl Into<String>,
        max_concurrency: usize,
        reconnect: ReconnectConfig,
        transport_credentials: Option<TransportCredentials>,
    ) -> Self {
        Self {
            namespaces: vec![String::from(DEFAULT_WORKER_NAMESPACE)],
            subject: String::from(DEFAULT_WORKER_SUBJECT),
            endpoint: endpoint.into(),
            task_queue: task_queue.into(),
            node: default_node(),
            identity: identity.into(),
            max_concurrency,
            reconnect,
            transport_credentials,
        }
    }
}

/// Builder for [`WorkerConfig`] with auth metadata defaults and explicit required fields.
#[derive(Clone, Debug, Default)]
pub struct WorkerConfigBuilder {
    namespaces: Option<Vec<String>>,
    subject: Option<String>,
    endpoint: Option<String>,
    task_queue: Option<String>,
    node: Option<String>,
    identity: Option<String>,
    max_concurrency: Option<usize>,
    reconnect_initial_backoff: Option<Duration>,
    reconnect_max_backoff: Option<Duration>,
    reconnect_max_attempts: Option<usize>,
    transport_credentials: Option<TransportCredentials>,
}

impl WorkerConfigBuilder {
    /// Creates an empty config builder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            namespaces: None,
            subject: None,
            endpoint: None,
            task_queue: None,
            node: None,
            identity: None,
            max_concurrency: None,
            reconnect_initial_backoff: None,
            reconnect_max_backoff: None,
            reconnect_max_attempts: None,
            transport_credentials: None,
        }
    }

    /// Sets the SET of namespaces advertised in worker stream authorization
    /// metadata and the registration scope. Replaces any previously set value.
    #[must_use]
    pub fn namespaces(mut self, namespaces: impl IntoIterator<Item = String>) -> Self {
        self.namespaces = Some(namespaces.into_iter().collect());
        self
    }

    /// Sets a single namespace, replacing any previously set namespace set.
    /// Convenience over [`Self::namespaces`] for the common one-namespace case.
    #[must_use]
    pub fn namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespaces = Some(vec![namespace.into()]);
        self
    }

    /// Sets the locality affinity (node) advertised at registration.
    #[must_use]
    pub fn node(mut self, node: impl Into<String>) -> Self {
        self.node = Some(node.into());
        self
    }

    /// Sets the subject advertised in worker stream authorization metadata.
    #[must_use]
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// Sets the engine worker endpoint URI.
    #[must_use]
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    /// Sets the task queue advertised to the engine.
    #[must_use]
    pub fn task_queue(mut self, task_queue: impl Into<String>) -> Self {
        self.task_queue = Some(task_queue.into());
        self
    }

    /// Sets the worker identity.
    #[must_use]
    pub fn identity(mut self, identity: impl Into<String>) -> Self {
        self.identity = Some(identity.into());
        self
    }

    /// Sets the operator-configured maximum concurrency.
    #[must_use]
    pub const fn max_concurrency(mut self, max_concurrency: usize) -> Self {
        self.max_concurrency = Some(max_concurrency);
        self
    }

    /// Sets the operator-configured initial reconnect backoff delay.
    #[must_use]
    pub const fn reconnect_initial_backoff(mut self, delay: Duration) -> Self {
        self.reconnect_initial_backoff = Some(delay);
        self
    }

    /// Sets the operator-configured reconnect backoff cap.
    #[must_use]
    pub const fn reconnect_max_backoff(mut self, delay: Duration) -> Self {
        self.reconnect_max_backoff = Some(delay);
        self
    }

    /// Sets the operator-configured maximum reconnect attempts.
    #[must_use]
    pub const fn reconnect_max_attempts(mut self, attempts: usize) -> Self {
        self.reconnect_max_attempts = Some(attempts);
        self
    }

    /// Sets optional opaque transport credentials.
    #[must_use]
    pub fn transport_credentials(mut self, credentials: TransportCredentials) -> Self {
        self.transport_credentials = Some(credentials);
        self
    }

    /// Builds a [`WorkerConfig`] when every required field has been supplied.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerConfigBuildError`] naming the missing required field.
    pub fn build(self) -> Result<WorkerConfig, WorkerConfigBuildError> {
        let namespaces = self
            .namespaces
            .filter(|namespaces| !namespaces.is_empty())
            .unwrap_or_else(|| vec![String::from(DEFAULT_WORKER_NAMESPACE)]);
        Ok(WorkerConfig {
            namespaces,
            subject: self
                .subject
                .unwrap_or_else(|| String::from(DEFAULT_WORKER_SUBJECT)),
            endpoint: self
                .endpoint
                .ok_or(WorkerConfigBuildError::MissingEndpoint)?,
            task_queue: self
                .task_queue
                .ok_or(WorkerConfigBuildError::MissingTaskQueue)?,
            node: self.node.unwrap_or_else(default_node),
            identity: self
                .identity
                .ok_or(WorkerConfigBuildError::MissingIdentity)?,
            max_concurrency: self
                .max_concurrency
                .ok_or(WorkerConfigBuildError::MissingMaxConcurrency)?,
            reconnect: ReconnectConfig {
                initial_backoff: self
                    .reconnect_initial_backoff
                    .ok_or(WorkerConfigBuildError::MissingReconnectInitialBackoff)?,
                max_backoff: self
                    .reconnect_max_backoff
                    .ok_or(WorkerConfigBuildError::MissingReconnectMaxBackoff)?,
                max_attempts: self
                    .reconnect_max_attempts
                    .ok_or(WorkerConfigBuildError::MissingReconnectMaxAttempts)?,
            },
            transport_credentials: self.transport_credentials,
        })
    }
}

/// Errors produced while building [`WorkerConfig`].
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerConfigBuildError {
    /// The endpoint was not supplied.
    #[error("worker endpoint is required")]
    MissingEndpoint,
    /// The task queue was not supplied.
    #[error("worker task queue is required")]
    MissingTaskQueue,
    /// The worker identity was not supplied.
    #[error("worker identity is required")]
    MissingIdentity,
    /// The max concurrency was not supplied.
    #[error("worker max_concurrency is required")]
    MissingMaxConcurrency,
    /// The reconnect initial backoff was not supplied.
    #[error("worker reconnect_initial_backoff is required")]
    MissingReconnectInitialBackoff,
    /// The reconnect max backoff was not supplied.
    #[error("worker reconnect_max_backoff is required")]
    MissingReconnectMaxBackoff,
    /// The reconnect max attempts value was not supplied.
    #[error("worker reconnect_max_attempts is required")]
    MissingReconnectMaxAttempts,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{TransportCredentials, WorkerConfig};

    #[test]
    fn worker_config_builder_round_trips_fields() -> Result<(), Box<dyn std::error::Error>> {
        let credentials = TransportCredentials::new(b"secret-token".to_vec());
        let config = WorkerConfig::builder()
            .endpoint("http://127.0.0.1:50051")
            .task_queue("payments")
            .identity("worker-a")
            .max_concurrency(7)
            .reconnect_initial_backoff(Duration::from_millis(10))
            .reconnect_max_backoff(Duration::from_millis(100))
            .reconnect_max_attempts(3)
            .namespace("payments")
            .subject("worker-a")
            .transport_credentials(credentials.clone())
            .build()?;

        assert_eq!(config.namespaces, vec![String::from("payments")]);
        assert_eq!(config.subject, "worker-a");
        assert_eq!(config.endpoint, "http://127.0.0.1:50051");
        assert_eq!(config.task_queue, "payments");
        assert!(!config.node.is_empty(), "node defaults to the hostname");
        assert_eq!(config.identity, "worker-a");
        assert_eq!(config.max_concurrency, 7);
        assert_eq!(config.reconnect.initial_backoff, Duration::from_millis(10));
        assert_eq!(config.reconnect.max_backoff, Duration::from_millis(100));
        assert_eq!(config.reconnect.max_attempts, 3);
        assert_eq!(config.transport_credentials, Some(credentials));
        assert!(!format!("{config:?}").contains("secret-token"));

        Ok(())
    }

    #[test]
    fn worker_config_new_uses_auth_metadata_defaults() {
        let config = WorkerConfig::new(
            "http://127.0.0.1:50051",
            "default",
            "worker-a",
            4,
            super::ReconnectConfig::new(Duration::from_millis(10), Duration::from_millis(100), 3),
            None,
        );

        assert_eq!(config.namespaces, vec![String::from("default")]);
        assert_eq!(config.subject, "worker");
        assert!(!config.node.is_empty(), "node defaults to the hostname");
    }

    #[test]
    fn worker_config_builder_carries_namespace_set_and_node()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = WorkerConfig::builder()
            .endpoint("http://127.0.0.1:50051")
            .task_queue("default")
            .identity("worker-a")
            .max_concurrency(1)
            .reconnect_initial_backoff(Duration::from_millis(5))
            .reconnect_max_backoff(Duration::from_millis(20))
            .reconnect_max_attempts(3)
            .namespaces([String::from("a"), String::from("b")])
            .node("host-7")
            .build()?;

        assert_eq!(
            config.namespaces,
            vec![String::from("a"), String::from("b")]
        );
        assert_eq!(config.node, "host-7");
        Ok(())
    }
}
