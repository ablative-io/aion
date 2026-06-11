//! `Client` and `ClientBuilder` connection, auth, and TLS support.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::error::ClientError;
use crate::handle::WorkflowHandle;
use crate::ops::StartFingerprint;
use crate::transport::{GrpcWorkflowTransport, WorkflowTransport};

/// Reusable caller-side SDK client for an `aion-server` deployment.
///
/// # Examples
///
/// ```no_run
/// # async fn connect() -> Result<(), aion_client::ClientError> {
/// use aion_client::{ClientAuth, ClientBuilder};
///
/// let client = ClientBuilder::new("https://aion.example.com")
///     .with_auth(ClientAuth::bearer("secret-token"))
///     .with_namespace("tenant-a")
///     .build()
///     .await?;
///
/// let shared = client.clone();
/// # let _ = shared;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Client {
    pub(crate) transport: Arc<dyn WorkflowTransport>,
    pub(crate) config: ClientConfig,
    idempotent_starts: Arc<Mutex<HashMap<String, (StartFingerprint, WorkflowHandle)>>>,
}

impl Client {
    /// Creates a builder for an `aion-server` endpoint.
    #[must_use]
    pub fn builder(endpoint: impl Into<String>) -> ClientBuilder {
        ClientBuilder::new(endpoint)
    }

    pub(crate) fn from_transport(
        config: ClientConfig,
        transport: Arc<dyn WorkflowTransport>,
    ) -> Self {
        Self {
            transport,
            config,
            idempotent_starts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[cfg(feature = "embedded")]
    /// Creates a client backed by an in-process embedded engine.
    #[must_use]
    pub fn embedded(engine: Arc<aion::Engine>) -> Self {
        let config = ClientConfig {
            endpoint: String::from("embedded://engine"),
            stream_endpoint: None,
            auth: None,
            tls: None,
            namespace: String::from("default"),
            subject: None,
            authorized_namespaces: Vec::new(),
        };
        Self::from_transport(
            config,
            Arc::new(crate::transport::EmbeddedWorkflowTransport::new(engine)),
        )
    }

    pub(crate) fn namespace(&self) -> &str {
        &self.config.namespace
    }

    pub(crate) async fn cached_start(
        &self,
        fingerprint: &StartFingerprint,
    ) -> Result<Option<WorkflowHandle>, ClientError> {
        let cache = self.idempotent_starts.lock().await;
        let Some((cached_fingerprint, handle)) = cache.get(fingerprint.key()) else {
            return Ok(None);
        };
        if cached_fingerprint == fingerprint {
            Ok(Some(handle.clone()))
        } else {
            Err(idempotency_conflict())
        }
    }

    pub(crate) async fn record_start(
        &self,
        fingerprint: StartFingerprint,
        handle: WorkflowHandle,
    ) -> Result<(), ClientError> {
        let mut cache = self.idempotent_starts.lock().await;
        match cache.get(fingerprint.key()) {
            Some((cached_fingerprint, _)) if cached_fingerprint == &fingerprint => Ok(()),
            Some(_) => Err(idempotency_conflict()),
            None => {
                cache.insert(fingerprint.key().to_owned(), (fingerprint, handle));
                Ok(())
            }
        }
    }
}

/// The SDK-boundary idempotency conflict: the same key was reused with a
/// different start request.
fn idempotency_conflict() -> ClientError {
    ClientError::already_exists(
        "idempotency key was already used by a different start request \
         (namespace, workflow type, or input differ)",
    )
}

/// Builder for [`Client`] connection, authentication, and TLS options.
#[derive(Clone, Debug)]
pub struct ClientBuilder {
    endpoint: String,
    stream_endpoint: Option<String>,
    auth: Option<ClientAuth>,
    tls: Option<TlsOptions>,
    namespace: String,
    subject: Option<String>,
    authorized_namespaces: Vec<String>,
}

impl ClientBuilder {
    /// Creates a builder for the supplied server endpoint.
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            stream_endpoint: None,
            auth: None,
            tls: None,
            namespace: String::from("default"),
            subject: None,
            authorized_namespaces: Vec::new(),
        }
    }

    /// Configures the WebSocket event-stream endpoint used by subscribe
    /// operations: the full URL of the server's `/events/stream` route, e.g.
    /// `ws://127.0.0.1:8080/events/stream` (`http`/`https` URLs are accepted
    /// and protocol-mapped to `ws`/`wss`).
    ///
    /// There is no default and nothing is derived: the gRPC endpoint and the
    /// HTTP/WebSocket listener are separate addresses. Subscribing without
    /// this option returns [`ClientError::InvalidArgument`] with a precise
    /// message.
    #[must_use]
    pub fn with_stream_endpoint(mut self, stream_endpoint: impl Into<String>) -> Self {
        self.stream_endpoint = Some(stream_endpoint.into());
        self
    }

    /// Configures the credential attached to every request.
    #[must_use]
    pub fn with_auth(mut self, auth: ClientAuth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Configures TLS options for the tonic channel.
    #[must_use]
    pub fn with_tls(mut self, tls: TlsOptions) -> Self {
        self.tls = Some(tls);
        self
    }

    /// Configures the namespace used by operations unless an operation option overrides it.
    #[must_use]
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    /// Configures the caller subject metadata sent to the server.
    #[must_use]
    pub fn with_subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// Configures the namespaces advertised in auth metadata.
    #[must_use]
    pub fn with_authorized_namespaces<I, S>(mut self, namespaces: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.authorized_namespaces = namespaces.into_iter().map(Into::into).collect();
        self
    }

    /// Connects once and returns a cheaply cloneable [`Client`].
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Unavailable`] for malformed endpoints and failed
    /// channel/TLS handshakes. Server-side credential rejection is surfaced as
    /// [`ClientError::Unauthenticated`] when AW returns gRPC `Unauthenticated`.
    pub async fn build(self) -> Result<Client, ClientError> {
        let config = ClientConfig::from(self);
        let transport = GrpcWorkflowTransport::connect(config.clone()).await?;
        Ok(Client::from_transport(config, Arc::new(transport)))
    }
}

/// Bearer authentication credential for server calls.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientAuth {
    token: String,
}

impl ClientAuth {
    /// Creates a bearer-token credential.
    #[must_use]
    pub fn bearer(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }
}

impl fmt::Debug for ClientAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientAuth")
            .field("token", &"<redacted>")
            .finish()
    }
}

/// TLS options for connecting to an HTTPS/TLS endpoint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsOptions {
    pub(crate) domain_name: Option<String>,
    pub(crate) ca_certificate_pem: Option<Vec<u8>>,
}

impl TlsOptions {
    /// Creates empty TLS options using platform/webpki roots.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the TLS domain name checked during the gRPC channel
    /// handshake. The WebSocket event stream always verifies against its own
    /// stream-endpoint host (`ClientBuilder::with_stream_endpoint`), so no
    /// override is needed there: point the stream endpoint at the name the
    /// server's certificate carries.
    #[must_use]
    pub fn with_domain_name(mut self, domain_name: impl Into<String>) -> Self {
        self.domain_name = Some(domain_name.into());
        self
    }

    /// Adds a PEM-encoded CA certificate trusted by BOTH transports: the
    /// tonic gRPC channel and the `wss://` WebSocket event stream.
    #[must_use]
    pub fn with_ca_certificate_pem(mut self, ca_certificate_pem: impl Into<Vec<u8>>) -> Self {
        self.ca_certificate_pem = Some(ca_certificate_pem.into());
        self
    }
}

/// Fully resolved client connection configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientConfig {
    pub(crate) endpoint: String,
    pub(crate) stream_endpoint: Option<String>,
    pub(crate) auth: Option<ClientAuth>,
    pub(crate) tls: Option<TlsOptions>,
    pub(crate) namespace: String,
    pub(crate) subject: Option<String>,
    pub(crate) authorized_namespaces: Vec<String>,
}

impl From<ClientBuilder> for ClientConfig {
    fn from(builder: ClientBuilder) -> Self {
        Self {
            endpoint: builder.endpoint,
            stream_endpoint: builder.stream_endpoint,
            auth: builder.auth,
            tls: builder.tls,
            namespace: builder.namespace,
            subject: builder.subject,
            authorized_namespaces: builder.authorized_namespaces,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Client, ClientAuth, ClientBuilder, ClientConfig, TlsOptions};

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn client_is_clone_send_sync() {
        assert_send_sync::<Client>();
    }

    #[test]
    fn auth_debug_redacts_token() {
        let auth = ClientAuth::bearer("secret-token");
        assert_eq!(format!("{auth:?}"), "ClientAuth { token: \"<redacted>\" }");
    }

    #[test]
    fn builder_captures_connection_options() {
        let config = ClientConfig::from(
            ClientBuilder::new("https://aion.example.com")
                .with_stream_endpoint("wss://aion-http.example.com/events/stream")
                .with_auth(ClientAuth::bearer("secret-token"))
                .with_tls(TlsOptions::new().with_domain_name("aion.example.com"))
                .with_namespace("tenant-a")
                .with_subject("alice")
                .with_authorized_namespaces(["tenant-a", "tenant-b"]),
        );

        assert_eq!(config.endpoint, "https://aion.example.com");
        assert_eq!(
            config.stream_endpoint,
            Some(String::from("wss://aion-http.example.com/events/stream"))
        );
        assert!(config.auth.is_some());
        assert!(config.tls.is_some());
        assert_eq!(config.namespace, "tenant-a");
        assert_eq!(config.subject, Some(String::from("alice")));
        assert_eq!(
            config.authorized_namespaces,
            vec![String::from("tenant-a"), String::from("tenant-b")]
        );
    }

    #[test]
    fn stream_endpoint_has_no_default() {
        let config = ClientConfig::from(ClientBuilder::new("https://aion.example.com"));
        assert_eq!(config.stream_endpoint, None);
    }
}
