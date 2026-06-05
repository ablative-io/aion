//! `WorkerConfig` endpoint, task queue, identity, concurrency, and TLS/credentials passthrough.

use std::fmt;

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

/// Operator-supplied worker connection and serving configuration.
///
/// No tunable field has a default. In particular, `max_concurrency` is provided
/// by the caller so the worker never assumes an implicit concurrency cap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerConfig {
    /// Engine worker endpoint URI.
    pub endpoint: String,
    /// Task queue advertised to the engine. The current AW wire names this field
    /// `namespace`; this SDK maps the task queue value to that owned wire shape.
    pub task_queue: String,
    /// Worker identity used by operators and future wire metadata.
    pub identity: String,
    /// Maximum concurrent activities this worker may serve.
    pub max_concurrency: usize,
    /// Opaque credentials for the transport implementation.
    pub transport_credentials: Option<TransportCredentials>,
}

impl WorkerConfig {
    /// Starts an explicit builder. The caller must provide every required field
    /// before calling [`WorkerConfigBuilder::build`].
    #[must_use]
    pub const fn builder() -> WorkerConfigBuilder {
        WorkerConfigBuilder::new()
    }

    /// Creates a worker config with every field supplied explicitly.
    #[must_use]
    pub fn new(
        endpoint: impl Into<String>,
        task_queue: impl Into<String>,
        identity: impl Into<String>,
        max_concurrency: usize,
        transport_credentials: Option<TransportCredentials>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            task_queue: task_queue.into(),
            identity: identity.into(),
            max_concurrency,
            transport_credentials,
        }
    }
}

/// Builder for [`WorkerConfig`] with no baked-in tunable defaults.
#[derive(Clone, Debug, Default)]
pub struct WorkerConfigBuilder {
    endpoint: Option<String>,
    task_queue: Option<String>,
    identity: Option<String>,
    max_concurrency: Option<usize>,
    transport_credentials: Option<TransportCredentials>,
}

impl WorkerConfigBuilder {
    /// Creates an empty config builder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            endpoint: None,
            task_queue: None,
            identity: None,
            max_concurrency: None,
            transport_credentials: None,
        }
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
}

#[cfg(test)]
mod tests {
    use super::{TransportCredentials, WorkerConfig};

    #[test]
    fn worker_config_builder_round_trips_fields() -> Result<(), Box<dyn std::error::Error>> {
        let credentials = TransportCredentials::new(b"secret-token".to_vec());
        let config = WorkerConfig::builder()
            .endpoint("http://127.0.0.1:50051")
            .task_queue("payments")
            .identity("worker-a")
            .max_concurrency(7)
            .transport_credentials(credentials.clone())
            .build()?;

        assert_eq!(config.endpoint, "http://127.0.0.1:50051");
        assert_eq!(config.task_queue, "payments");
        assert_eq!(config.identity, "worker-a");
        assert_eq!(config.max_concurrency, 7);
        assert_eq!(config.transport_credentials, Some(credentials));
        assert!(!format!("{config:?}").contains("secret-token"));

        Ok(())
    }
}
