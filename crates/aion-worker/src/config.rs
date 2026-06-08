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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconnectConfig {
    /// Initial reconnect backoff delay. Must be non-zero before reconnecting.
    pub initial_backoff: Duration,
    /// Maximum reconnect backoff delay cap. Must be non-zero before reconnecting.
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
/// Most tunable fields are caller-supplied. Namespace authorization metadata
/// defaults to `default`/`worker` so development workers can register against
/// the default task queue without an explicit auth setup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerConfig {
    /// Namespace advertised in `x-aion-namespaces` worker stream metadata.
    pub namespace: String,
    /// Subject advertised in `x-aion-subject` worker stream metadata.
    pub subject: String,
    /// Engine worker endpoint URI.
    pub endpoint: String,
    /// Task queue advertised to the engine. The current AW wire names this field
    /// `namespace`; this SDK maps the task queue value to that owned wire shape.
    pub task_queue: String,
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
            namespace: String::from(DEFAULT_WORKER_NAMESPACE),
            subject: String::from(DEFAULT_WORKER_SUBJECT),
            endpoint: endpoint.into(),
            task_queue: task_queue.into(),
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
    namespace: Option<String>,
    subject: Option<String>,
    endpoint: Option<String>,
    task_queue: Option<String>,
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
            namespace: None,
            subject: None,
            endpoint: None,
            task_queue: None,
            identity: None,
            max_concurrency: None,
            reconnect_initial_backoff: None,
            reconnect_max_backoff: None,
            reconnect_max_attempts: None,
            transport_credentials: None,
        }
    }

    /// Sets the namespace advertised in worker stream authorization metadata.
    #[must_use]
    pub fn namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = Some(namespace.into());
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
        Ok(WorkerConfig {
            namespace: self
                .namespace
                .unwrap_or_else(|| String::from(DEFAULT_WORKER_NAMESPACE)),
            subject: self
                .subject
                .unwrap_or_else(|| String::from(DEFAULT_WORKER_SUBJECT)),
            endpoint: self
                .endpoint
                .ok_or(WorkerConfigBuildError::MissingEndpoint)?,
            task_queue: self
                .task_queue
                .ok_or(WorkerConfigBuildError::MissingTaskQueue)?,
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

        assert_eq!(config.namespace, "payments");
        assert_eq!(config.subject, "worker-a");
        assert_eq!(config.endpoint, "http://127.0.0.1:50051");
        assert_eq!(config.task_queue, "payments");
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

        assert_eq!(config.namespace, "default");
        assert_eq!(config.subject, "worker");
    }
}
