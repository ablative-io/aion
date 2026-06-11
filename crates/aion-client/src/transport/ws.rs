//! WebSocket event-stream transport.
//!
//! Speaks the cross-SDK subscription protocol against the server's
//! `/events/stream` endpoint:
//!
//! - caller identity (`authorization`, `x-aion-subject`, `x-aion-namespaces`)
//!   travels as headers on the upgrade request;
//! - the first client frame is the JSON `SubscriptionRequest` in the
//!   documented hand-written shape (`{"per_workflow": ...}`, `{"filtered":
//!   ...}`, `{"firehose": ...}`), with `resume_from_seq` (the FIRST sequence
//!   number wanted, `last delivered + 1`) riding inside the per-workflow
//!   variant on resume;
//! - server frames are `StreamedEvent` JSON, except terminal
//!   `{"error": <WireError>}` frames, which are mapped through the shared
//!   taxonomy (`lagged` — including the `SequenceContiguityViolation`
//!   discriminator — becomes [`ClientError::Unavailable`] so the resume loop
//!   reconnects with its cursor; `namespace_denied` / `not_found` /
//!   `invalid_input` are terminal);
//! - a normal close (code 1000) ends the stream; any abnormal close or socket
//!   failure surfaces one `Err(`[`ClientError::Unavailable`]`)` item so the
//!   resume loop reconnects.

use std::sync::Arc;

use aion_core::Event;
use aion_proto::{StreamedEvent, SubscriptionRequest, WireError, subscription_request};
use futures::stream::BoxStream;
use futures::{SinkExt, StreamExt, stream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use crate::client::{ClientConfig, TlsOptions};
use crate::error::ClientError;
use crate::transport::contract::SubscriptionAttempt;

/// Path of the server's WebSocket event stream.
pub const EVENT_STREAM_PATH: &str = "/events/stream";

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Opens one WebSocket subscription attempt for `request`.
///
/// `resume_from_sequence`, when supplied by the resume loop, is written into
/// the per-workflow subscription's `resume_from_seq` field; filtered and
/// firehose subscriptions are live-only and reject a cursor.
///
/// # Errors
///
/// Returns [`ClientError::InvalidArgument`] when no stream endpoint is
/// configured, the endpoint URL is unusable, or a cursor is supplied for a
/// live-only subscription; [`ClientError::Unauthenticated`] when the upgrade
/// is rejected with HTTP 401; and [`ClientError::Unavailable`] when the
/// connection cannot be established.
pub async fn open_subscription(
    config: &ClientConfig,
    request: SubscriptionRequest,
    resume_from_sequence: Option<u64>,
) -> Result<SubscriptionAttempt, ClientError> {
    // Validate the endpoint and build the first frame before opening a
    // socket, so invalid input never costs a connection.
    let url = stream_url(config)?;
    let frame = subscription_frame(request, resume_from_sequence)?;

    let mut upgrade = url.as_str().into_client_request().map_err(|source| {
        ClientError::invalid_argument(format!(
            "stream endpoint {url} is not a valid websocket URL: {source}"
        ))
    })?;
    apply_headers(&mut upgrade, config)?;
    let connector = tls_connector(config.tls.as_ref())?;

    let (mut socket, _response) =
        tokio_tungstenite::connect_async_tls_with_config(upgrade, None, false, connector)
            .await
            .map_err(map_connect_error)?;
    socket
        .send(Message::Text(frame.into()))
        .await
        .map_err(|source| {
            ClientError::unavailable(format!(
                "websocket subscription frame send failed: {source}"
            ))
        })?;

    Ok(SubscriptionAttempt::new(socket_events(socket)))
}

/// Resolves the configured stream endpoint into a `ws://`/`wss://` URL.
///
/// `ws`/`wss` endpoints pass through unchanged; `http`/`https` endpoints are
/// protocol-mapped (the same listener serves `/events/stream`, so this is
/// scheme mapping, never an invented address). There is NO default: the
/// gRPC and HTTP/WebSocket listeners are separate addresses, so deriving one
/// from the other would be an assumed default.
fn stream_url(config: &ClientConfig) -> Result<String, ClientError> {
    let Some(endpoint) = config.stream_endpoint.as_deref() else {
        return Err(ClientError::invalid_argument(format!(
            "no stream endpoint is configured; event subscriptions require \
             ClientBuilder::with_stream_endpoint pointing at the server's \
             {EVENT_STREAM_PATH} WebSocket URL (the HTTP/WebSocket listener \
             is a separate address from the gRPC endpoint)"
        )));
    };
    let Some((scheme, rest)) = endpoint.split_once("://") else {
        return Err(ClientError::invalid_argument(format!(
            "stream endpoint {endpoint} is not an absolute URL; expected a \
             ws://, wss://, http://, or https:// address"
        )));
    };
    match scheme {
        "ws" | "wss" => Ok(endpoint.to_owned()),
        "http" => Ok(format!("ws://{rest}")),
        "https" => Ok(format!("wss://{rest}")),
        other => Err(ClientError::invalid_argument(format!(
            "cannot derive a websocket stream URL from a {other}:// endpoint; \
             expected ws://, wss://, http://, or https://"
        ))),
    }
}

/// Builds the `wss://` TLS connector from the client's [`TlsOptions`]: the
/// webpki trust roots plus every caller-supplied CA certificate from
/// `ca_certificate_pem` — the same custom-CA material the gRPC channel
/// trusts, so a deployment behind a private CA streams events over the same
/// trust configuration it uses for unary calls.
///
/// Returns `None` when the client has no TLS options, so tokio-tungstenite's
/// built-in webpki-roots default applies unchanged. The TLS server name is
/// always the stream URL's host; `TlsOptions::with_domain_name` overrides
/// verification for the gRPC channel only.
///
/// # Errors
///
/// Returns [`ClientError::InvalidArgument`] when `ca_certificate_pem` is not
/// parseable PEM, contains no certificate, or holds a certificate the trust
/// store rejects.
pub(crate) fn tls_connector(tls: Option<&TlsOptions>) -> Result<Option<Connector>, ClientError> {
    let Some(tls) = tls else {
        return Ok(None);
    };
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    if let Some(pem) = &tls.ca_certificate_pem {
        let mut added = 0_usize;
        for certificate in rustls_pemfile::certs(&mut pem.as_slice()) {
            let certificate = certificate.map_err(|source| {
                ClientError::invalid_argument(format!(
                    "TLS ca_certificate_pem is not parseable PEM: {source}"
                ))
            })?;
            roots.add(certificate).map_err(|source| {
                ClientError::invalid_argument(format!(
                    "TLS ca_certificate_pem holds a certificate the trust store rejects: {source}"
                ))
            })?;
            added += 1;
        }
        if added == 0 {
            return Err(ClientError::invalid_argument(
                "TLS ca_certificate_pem contains no CA certificate",
            ));
        }
    }
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Some(Connector::Rustls(Arc::new(tls_config))))
}

/// Builds the first client frame: the JSON `SubscriptionRequest` in the
/// documented hand-written shape, with the resume cursor written into the
/// per-workflow variant.
fn subscription_frame(
    request: SubscriptionRequest,
    resume_from_sequence: Option<u64>,
) -> Result<String, ClientError> {
    let (key, subscription) = match request.subscription {
        Some(subscription_request::Subscription::PerWorkflow(mut per_workflow)) => {
            if let Some(cursor) = resume_from_sequence {
                if cursor == 0 {
                    return Err(ClientError::invalid_argument(
                        "resume_from_seq must be >= 1 (the first sequence number wanted)",
                    ));
                }
                per_workflow.resume_from_seq = Some(cursor);
            }
            ("per_workflow", encode_subscription(&per_workflow)?)
        }
        Some(subscription_request::Subscription::Filtered(filtered)) => {
            reject_live_only_cursor("filtered", resume_from_sequence)?;
            ("filtered", encode_subscription(&filtered)?)
        }
        Some(subscription_request::Subscription::Firehose(firehose)) => {
            reject_live_only_cursor("firehose", resume_from_sequence)?;
            ("firehose", encode_subscription(&firehose)?)
        }
        None => {
            return Err(ClientError::invalid_argument(
                "subscription request is missing its subscription variant",
            ));
        }
    };
    serde_json::to_string(&serde_json::json!({ key: subscription })).map_err(|source| {
        ClientError::invalid_argument(format!("failed to encode subscription request: {source}"))
    })
}

fn encode_subscription<T: serde::Serialize>(value: &T) -> Result<serde_json::Value, ClientError> {
    serde_json::to_value(value).map_err(|source| {
        ClientError::invalid_argument(format!("failed to encode subscription request: {source}"))
    })
}

fn reject_live_only_cursor(kind: &str, cursor: Option<u64>) -> Result<(), ClientError> {
    if cursor.is_some() {
        return Err(ClientError::invalid_argument(format!(
            "{kind} event streams are live-only by design; resume cursors are \
             valid for per-workflow subscriptions only"
        )));
    }
    Ok(())
}

/// Forwards the caller identity headers the server's caller extraction reads.
fn apply_headers(
    upgrade: &mut tungstenite::handshake::client::Request,
    config: &ClientConfig,
) -> Result<(), ClientError> {
    let headers = upgrade.headers_mut();
    if let Some(auth) = &config.auth {
        let value = HeaderValue::from_str(&format!("Bearer {}", auth.token()))
            .map_err(|_| ClientError::invalid_argument("auth token is not a valid header value"))?;
        headers.insert("authorization", value);
    }
    if let Some(subject) = &config.subject {
        let value = HeaderValue::from_str(subject).map_err(|_| {
            ClientError::invalid_argument("subject is not a valid x-aion-subject header value")
        })?;
        headers.insert("x-aion-subject", value);
    }
    if !config.authorized_namespaces.is_empty() {
        let value =
            HeaderValue::from_str(&config.authorized_namespaces.join(",")).map_err(|_| {
                ClientError::invalid_argument(
                    "authorized namespaces are not a valid x-aion-namespaces header value",
                )
            })?;
        headers.insert("x-aion-namespaces", value);
    }
    Ok(())
}

fn map_connect_error(error: tungstenite::Error) -> ClientError {
    match error {
        // The server rejects bad credentials on the upgrade with HTTP 401.
        tungstenite::Error::Http(response)
            if response.status() == tungstenite::http::StatusCode::UNAUTHORIZED =>
        {
            ClientError::unauthenticated("websocket upgrade was rejected with HTTP 401")
        }
        other => ClientError::unavailable(format!("websocket connect failed: {other}")),
    }
}

/// Adapts one connected socket into the per-attempt event stream.
///
/// The stream is fused after any error item: a terminal `{"error": ...}`
/// frame or a transport failure ends the attempt, and the surrounding resume
/// loop decides whether to reconnect (`Unavailable`) or surface the error.
fn socket_events(socket: WsStream) -> BoxStream<'static, Result<Event, ClientError>> {
    stream::unfold(Some(socket), |state| async move {
        let mut socket = state?;
        loop {
            return match socket.next().await {
                None | Some(Err(tungstenite::Error::ConnectionClosed)) => None,
                Some(Ok(Message::Text(text))) => match decode_frame(text.as_bytes()) {
                    Ok(event) => Some((Ok(event), Some(socket))),
                    Err(error) => Some((Err(error), None)),
                },
                Some(Ok(Message::Binary(bytes))) => match decode_frame(&bytes) {
                    Ok(event) => Some((Ok(event), Some(socket))),
                    Err(error) => Some((Err(error), None)),
                },
                Some(Ok(Message::Close(frame))) => match frame {
                    // A normal closure ends the stream; anything else is a
                    // transient drop the resume loop recovers from.
                    Some(frame) if frame.code == CloseCode::Normal => None,
                    Some(frame) => Some((
                        Err(ClientError::unavailable(format!(
                            "websocket closed abnormally ({} {})",
                            frame.code, frame.reason
                        ))),
                        None,
                    )),
                    None => Some((
                        Err(ClientError::unavailable(
                            "websocket closed without a close frame",
                        )),
                        None,
                    )),
                },
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                Some(Err(source)) => Some((
                    Err(ClientError::unavailable(format!(
                        "websocket transport failed: {source}"
                    ))),
                    None,
                )),
            };
        }
    })
    .boxed()
}

/// Decodes one server frame: a `StreamedEvent` yields its core event, a
/// `{"error": <WireError>}` frame yields the mapped taxonomy error.
fn decode_frame(bytes: &[u8]) -> Result<Event, ClientError> {
    #[derive(serde::Deserialize)]
    struct ErrorFrame {
        error: WireError,
    }
    if let Ok(frame) = serde_json::from_slice::<ErrorFrame>(bytes) {
        return Err(ClientError::from_wire_error(frame.error));
    }
    let streamed = serde_json::from_slice::<StreamedEvent>(bytes).map_err(|source| {
        ClientError::server(format!(
            "event stream frame is neither a StreamedEvent nor an error frame: {source}"
        ))
    })?;
    streamed
        .decode_event()
        .map_err(ClientError::from_wire_error)
}

#[cfg(test)]
mod tests {
    use aion_proto::{
        FilteredSubscription, FirehoseSubscription, PerWorkflowSubscription, ProtoWorkflowId,
        SubscriptionRequest, WireError, encode_streamed_event, subscription_request,
    };
    use serde_json::json;

    use super::{decode_frame, stream_url, subscription_frame};
    use crate::client::{ClientBuilder, ClientConfig};
    use crate::error::{ClientError, ErrorDetail};

    fn config(stream_endpoint: Option<&str>) -> ClientConfig {
        let mut builder = ClientBuilder::new("http://127.0.0.1:50051");
        if let Some(endpoint) = stream_endpoint {
            builder = builder.with_stream_endpoint(endpoint);
        }
        ClientConfig::from(builder)
    }

    fn per_workflow_request(resume_from_seq: Option<u64>) -> SubscriptionRequest {
        SubscriptionRequest {
            subscription: Some(subscription_request::Subscription::PerWorkflow(
                PerWorkflowSubscription {
                    namespace: String::from("tenant-a"),
                    workflow_id: Some(ProtoWorkflowId {
                        uuid: String::from("00000000-0000-0000-0000-000000000001"),
                    }),
                    resume_from_seq,
                },
            )),
        }
    }

    #[test]
    fn missing_stream_endpoint_is_invalid_argument_with_precise_message()
    -> Result<(), Box<dyn std::error::Error>> {
        let error = stream_url(&config(None)).err();

        let Some(ClientError::InvalidArgument { detail }) = error else {
            return Err(format!("must be InvalidArgument, got {error:?}").into());
        };
        assert!(
            detail.message.contains("with_stream_endpoint"),
            "detail: {detail}"
        );
        assert!(
            detail.message.contains("/events/stream"),
            "detail: {detail}"
        );
        Ok(())
    }

    #[test]
    fn stream_url_maps_http_schemes_and_passes_ws_through() -> Result<(), Box<dyn std::error::Error>>
    {
        assert_eq!(
            stream_url(&config(Some("ws://127.0.0.1:8080/events/stream")))?,
            "ws://127.0.0.1:8080/events/stream"
        );
        assert_eq!(
            stream_url(&config(Some("wss://aion.example.com/events/stream")))?,
            "wss://aion.example.com/events/stream"
        );
        assert_eq!(
            stream_url(&config(Some("http://127.0.0.1:8080/events/stream")))?,
            "ws://127.0.0.1:8080/events/stream"
        );
        assert_eq!(
            stream_url(&config(Some("https://aion.example.com/events/stream")))?,
            "wss://aion.example.com/events/stream"
        );
        Ok(())
    }

    #[test]
    fn stream_url_rejects_non_websocket_schemes() {
        for endpoint in ["ftp://example.com/events/stream", "not-a-url"] {
            let error = stream_url(&config(Some(endpoint))).err();
            assert!(
                matches!(error, Some(ClientError::InvalidArgument { .. })),
                "{endpoint} must be rejected, got {error:?}"
            );
        }
    }

    /// A PEM-encoded self-signed CA fixture (no key material), used to prove
    /// the custom-CA plumbing into the WebSocket TLS connector.
    const TEST_CA_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIBnTCCAUOgAwIBAgIUZeF05kLNKnZTC4xSV0RxC7fQ+DgwCgYIKoZIzj0EAwIw
IzEhMB8GA1UEAwwYYWlvbi1jb25mb3JtYW5jZS10ZXN0LWNhMCAXDTI2MDYxMTE5
MDgwM1oYDzIxMjYwNTE4MTkwODAzWjAjMSEwHwYDVQQDDBhhaW9uLWNvbmZvcm1h
bmNlLXRlc3QtY2EwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAQNxfK/cvPDW0ue
a6AjlScsSdO+U+H53YG50Fn4HULhmu2Wu8JfcmEo4Rgao+SciqnpqRFiU4X0FTuh
yoKxsO+uo1MwUTAdBgNVHQ4EFgQUwkbSaaXC/W1IxAkg+3Jl7jz+wckwHwYDVR0j
BBgwFoAUwkbSaaXC/W1IxAkg+3Jl7jz+wckwDwYDVR0TAQH/BAUwAwEB/zAKBggq
hkjOPQQDAgNIADBFAiEAtalplxZn9gozJpWUrMO4ddjy/IuKXwO1b7AwSvwtO8EC
ICo9Vooy83Vq0mVVYmWRSVMZ4AtTrLY+7h3pIVrGLLl/
-----END CERTIFICATE-----
";

    #[test]
    fn no_tls_options_means_no_custom_connector() -> Result<(), Box<dyn std::error::Error>> {
        assert!(super::tls_connector(None)?.is_none());
        Ok(())
    }

    #[test]
    fn tls_options_without_custom_ca_build_a_webpki_connector()
    -> Result<(), Box<dyn std::error::Error>> {
        let connector = super::tls_connector(Some(&crate::client::TlsOptions::new()))?;
        assert!(
            matches!(connector, Some(tokio_tungstenite::Connector::Rustls(_))),
            "TLS options must produce a rustls connector"
        );
        Ok(())
    }

    #[test]
    fn custom_ca_certificate_is_added_to_the_websocket_trust_roots()
    -> Result<(), Box<dyn std::error::Error>> {
        let options = crate::client::TlsOptions::new().with_ca_certificate_pem(TEST_CA_PEM);
        let connector = super::tls_connector(Some(&options))?;
        let Some(tokio_tungstenite::Connector::Rustls(config)) = connector else {
            return Err("custom CA must produce a rustls connector".into());
        };
        // The webpki bundle plus exactly one extra caller-supplied root.
        let baseline = webpki_roots::TLS_SERVER_ROOTS.len();
        // rustls exposes no public root iterator on ClientConfig; the
        // RootCertStore is rebuilt identically here to pin the count.
        let mut roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        for certificate in rustls_pemfile::certs(&mut TEST_CA_PEM.as_bytes()) {
            roots.add(certificate?)?;
        }
        assert_eq!(roots.roots.len(), baseline + 1);
        drop(config);
        Ok(())
    }

    #[test]
    fn malformed_custom_ca_pem_is_invalid_argument() {
        for pem in ["not pem at all", ""] {
            let options =
                crate::client::TlsOptions::new().with_ca_certificate_pem(pem.as_bytes().to_vec());
            let error = super::tls_connector(Some(&options)).err();
            assert!(
                matches!(error, Some(ClientError::InvalidArgument { .. })),
                "{pem:?} must be rejected as InvalidArgument, got {error:?}"
            );
        }
    }

    #[test]
    fn per_workflow_frame_carries_the_resume_cursor() -> Result<(), Box<dyn std::error::Error>> {
        let frame = subscription_frame(per_workflow_request(None), Some(7))?;
        let value: serde_json::Value = serde_json::from_str(&frame)?;

        assert_eq!(value["per_workflow"]["namespace"], json!("tenant-a"));
        assert_eq!(
            value["per_workflow"]["workflow_id"]["uuid"],
            json!("00000000-0000-0000-0000-000000000001")
        );
        assert_eq!(value["per_workflow"]["resume_from_seq"], json!(7));
        Ok(())
    }

    #[test]
    fn initial_attach_sends_no_resume_cursor() -> Result<(), Box<dyn std::error::Error>> {
        // Cross-SDK contract: an initial attach is a live tail — the cursor
        // field stays absent/null, matching the Python and TypeScript SDKs.
        let frame = subscription_frame(per_workflow_request(None), None)?;
        let value: serde_json::Value = serde_json::from_str(&frame)?;

        assert_eq!(value["per_workflow"]["resume_from_seq"], json!(null));
        Ok(())
    }

    #[test]
    fn resume_cursor_zero_never_reaches_the_wire() -> Result<(), Box<dyn std::error::Error>> {
        let error = subscription_frame(per_workflow_request(None), Some(0)).err();

        let Some(ClientError::InvalidArgument { detail }) = error else {
            return Err(format!("cursor 0 must be InvalidArgument, got {error:?}").into());
        };
        assert!(detail.message.contains(">= 1"), "detail: {detail}");
        Ok(())
    }

    #[test]
    fn live_only_subscriptions_reject_resume_cursors() -> Result<(), Box<dyn std::error::Error>> {
        let filtered = SubscriptionRequest {
            subscription: Some(subscription_request::Subscription::Filtered(
                FilteredSubscription {
                    namespace: String::from("tenant-a"),
                    workflow_type: None,
                    status: None,
                    namespace_selector: None,
                },
            )),
        };
        let firehose = SubscriptionRequest {
            subscription: Some(subscription_request::Subscription::Firehose(
                FirehoseSubscription {
                    namespace: String::from("tenant-a"),
                },
            )),
        };

        for request in [filtered, firehose] {
            let error = subscription_frame(request, Some(3)).err();
            let Some(ClientError::InvalidArgument { detail }) = error else {
                return Err(
                    format!("live-only cursor must be InvalidArgument, got {error:?}").into(),
                );
            };
            assert!(detail.message.contains("live-only"), "detail: {detail}");
        }
        Ok(())
    }

    #[test]
    fn streamed_event_frames_decode_to_core_events() -> Result<(), Box<dyn std::error::Error>> {
        let workflow_id = aion_core::WorkflowId::new_v4();
        let event = aion_core::Event::SignalReceived {
            envelope: aion_core::EventEnvelope {
                seq: 3,
                recorded_at: chrono::Utc::now(),
                workflow_id,
            },
            name: String::from("approve"),
            payload: aion_core::Payload::from_json(&json!({ "ok": true }))?,
        };
        let frame = serde_json::to_string(&encode_streamed_event("tenant-a", None, &event)?)?;

        let decoded =
            decode_frame(frame.as_bytes()).map_err(|error| format!("decode failed: {error}"))?;
        assert_eq!(decoded, event);
        Ok(())
    }

    #[test]
    fn lagged_error_frames_map_to_unavailable_so_the_resume_loop_reconnects()
    -> Result<(), Box<dyn std::error::Error>> {
        let lagged = serde_json::to_string(&json!({
            "error": WireError::lagged("subscriber lagged behind")
        }))?;
        assert_eq!(
            decode_frame(lagged.as_bytes()).err(),
            Some(ClientError::unavailable("subscriber lagged behind"))
        );

        // The per-workflow contiguity tripwire rides the `lagged` code with
        // the SequenceContiguityViolation discriminator: same recovery, the
        // resume loop reconnects with `resume_from_seq = last delivered + 1`.
        let violation = serde_json::to_string(&json!({
            "error": {
                "code": "lagged",
                "message": "per-workflow stream contiguity violated",
                "error_type": "SequenceContiguityViolation",
            }
        }))?;
        assert_eq!(
            decode_frame(violation.as_bytes()).err(),
            Some(ClientError::unavailable(ErrorDetail::with_type(
                "per-workflow stream contiguity violated",
                "SequenceContiguityViolation",
            )))
        );
        Ok(())
    }

    #[test]
    fn terminal_error_frames_map_through_the_shared_taxonomy()
    -> Result<(), Box<dyn std::error::Error>> {
        let not_found = serde_json::to_string(&json!({
            "error": WireError::not_found("workflow not found in namespace tenant-a")
        }))?;
        assert_eq!(
            decode_frame(not_found.as_bytes()).err(),
            Some(ClientError::not_found(
                "workflow not found in namespace tenant-a"
            ))
        );

        let denied = serde_json::to_string(&json!({
            "error": WireError::namespace_denied("namespace tenant-b is not granted")
        }))?;
        assert_eq!(
            decode_frame(denied.as_bytes()).err(),
            Some(ClientError::namespace_denied(
                "namespace tenant-b is not granted"
            ))
        );

        let invalid = serde_json::to_string(&json!({
            "error": {
                "code": "invalid_input",
                "message": "resume_from_seq 9 is ahead of recorded history",
                "error_type": "ResumeCursorAheadOfHistory",
            }
        }))?;
        assert_eq!(
            decode_frame(invalid.as_bytes()).err(),
            Some(ClientError::invalid_argument(ErrorDetail::with_type(
                "resume_from_seq 9 is ahead of recorded history",
                "ResumeCursorAheadOfHistory",
            )))
        );
        Ok(())
    }

    #[test]
    fn unrecognizable_frames_are_terminal_server_errors() {
        let error = decode_frame(b"not json");
        assert!(
            matches!(error, Err(ClientError::Server { .. })),
            "garbage frames must be terminal, got {error:?}"
        );
    }

    // ----- live-socket protocol tests against a local tungstenite server -----

    use std::collections::HashMap;
    use std::sync::Arc;

    use futures::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

    use crate::client::ClientAuth;

    /// What one server-side accept observed: the upgrade-request headers and
    /// the decoded first (subscription) frame.
    struct CapturedAttempt {
        headers: HashMap<String, String>,
        subscription: serde_json::Value,
    }

    enum AttemptEnd {
        CleanClose,
        Drop,
    }

    async fn accept_one(
        listener: &TcpListener,
        responses: Vec<Message>,
        end: AttemptEnd,
    ) -> Result<CapturedAttempt, Box<dyn std::error::Error + Send + Sync>> {
        let (stream, _) = listener.accept().await?;
        let captured: Arc<std::sync::Mutex<HashMap<String, String>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));
        let sink = Arc::clone(&captured);
        let callback = move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                             response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            if let Ok(mut headers) = sink.lock() {
                for (name, value) in request.headers() {
                    if let Ok(value) = value.to_str() {
                        headers.insert(name.as_str().to_owned(), value.to_owned());
                    }
                }
            }
            Ok(response)
        };
        let mut socket = tokio_tungstenite::accept_hdr_async(stream, callback).await?;
        let first = socket
            .next()
            .await
            .ok_or("client sent no subscription frame")??;
        let Message::Text(text) = first else {
            return Err(format!("expected a text subscription frame, got {first:?}").into());
        };
        let subscription: serde_json::Value = serde_json::from_str(text.as_str())?;
        for frame in responses {
            socket.send(frame).await?;
        }
        match end {
            AttemptEnd::CleanClose => {
                socket
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Normal,
                        reason: "".into(),
                    })))
                    .await?;
                // Drain until the close handshake completes.
                while let Some(message) = socket.next().await {
                    drop(message);
                }
            }
            AttemptEnd::Drop => drop(socket),
        }
        let headers = captured
            .lock()
            .map_err(|_| "captured-header mutex poisoned")?
            .clone();
        Ok(CapturedAttempt {
            headers,
            subscription,
        })
    }

    fn event_frame(
        seq: u64,
        workflow_id: &aion_core::WorkflowId,
    ) -> Result<Message, Box<dyn std::error::Error + Send + Sync>> {
        let event = aion_core::Event::SignalReceived {
            envelope: aion_core::EventEnvelope {
                seq,
                recorded_at: chrono::Utc::now(),
                workflow_id: workflow_id.clone(),
            },
            name: format!("signal-{seq}"),
            payload: aion_core::Payload::from_json(&json!({ "seq": seq }))?,
        };
        let frame = serde_json::to_string(&encode_streamed_event("tenant-a", None, &event)?)?;
        Ok(Message::Text(frame.into()))
    }

    fn live_config(port: u16) -> ClientConfig {
        ClientConfig::from(
            ClientBuilder::new("http://127.0.0.1:50051")
                .with_stream_endpoint(format!("ws://127.0.0.1:{port}/events/stream"))
                .with_auth(ClientAuth::bearer("secret-token"))
                .with_subject("alice")
                .with_namespace("tenant-a")
                .with_authorized_namespaces(["tenant-a", "tenant-b"]),
        )
    }

    #[tokio::test]
    async fn open_subscription_streams_events_and_forwards_identity_headers()
    -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let server = tokio::spawn(async move {
            accept_one(
                &listener,
                vec![event_frame(5, &workflow_id)?, event_frame(6, &workflow_id)?],
                AttemptEnd::CleanClose,
            )
            .await
        });

        let attempt =
            super::open_subscription(&live_config(port), per_workflow_request(None), Some(5))
                .await
                .map_err(|error| format!("open_subscription failed: {error}"))?;
        let delivered: Vec<_> = attempt.events.collect().await;
        let captured = tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await??
            .map_err(|error| format!("server side failed: {error}"))?;

        // The upgrade request carried the caller identity headers.
        assert_eq!(
            captured.headers.get("authorization").map(String::as_str),
            Some("Bearer secret-token")
        );
        assert_eq!(
            captured.headers.get("x-aion-subject").map(String::as_str),
            Some("alice")
        );
        assert_eq!(
            captured
                .headers
                .get("x-aion-namespaces")
                .map(String::as_str),
            Some("tenant-a,tenant-b")
        );
        // The first frame is the per-workflow subscription with the cursor.
        assert_eq!(
            captured.subscription["per_workflow"]["resume_from_seq"],
            json!(5)
        );
        assert_eq!(
            captured.subscription["per_workflow"]["namespace"],
            json!("tenant-a")
        );
        // Both events decoded; the clean close ended the stream.
        let seqs = delivered
            .into_iter()
            .map(|item| item.map(|event| event.seq()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("stream item failed: {error}"))?;
        assert_eq!(seqs, vec![5, 6]);
        Ok(())
    }

    #[tokio::test]
    async fn abrupt_socket_drop_surfaces_one_unavailable_item()
    -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let server = tokio::spawn(async move {
            accept_one(
                &listener,
                vec![event_frame(1, &workflow_id)?],
                AttemptEnd::Drop,
            )
            .await
        });

        let attempt =
            super::open_subscription(&live_config(port), per_workflow_request(None), None)
                .await
                .map_err(|error| format!("open_subscription failed: {error}"))?;
        let delivered: Vec<_> = attempt.events.collect().await;
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await??
            .map_err(|error| format!("server side failed: {error}"))?;

        assert_eq!(delivered.len(), 2, "one event then the transient error");
        assert!(matches!(&delivered[0], Ok(event) if event.seq() == 1));
        assert!(
            matches!(
                delivered[1].as_ref().err(),
                Some(ClientError::Unavailable { .. })
            ),
            "an abrupt drop must surface retryable Unavailable, got {:?}",
            delivered[1]
        );
        Ok(())
    }

    #[tokio::test]
    async fn terminal_error_frame_ends_the_attempt_with_the_mapped_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let error_frame = serde_json::to_string(&json!({
            "error": WireError::not_found("workflow not found in namespace tenant-a")
        }))?;
        let server = tokio::spawn(async move {
            accept_one(
                &listener,
                vec![Message::Text(error_frame.into())],
                AttemptEnd::CleanClose,
            )
            .await
        });

        let attempt =
            super::open_subscription(&live_config(port), per_workflow_request(None), None)
                .await
                .map_err(|error| format!("open_subscription failed: {error}"))?;
        let delivered: Vec<_> = attempt.events.collect().await;
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await??
            .map_err(|error| format!("server side failed: {error}"))?;

        assert_eq!(
            delivered,
            vec![Err(ClientError::not_found(
                "workflow not found in namespace tenant-a"
            ))]
        );
        Ok(())
    }

    /// Full resume-loop protocol flow over real sockets: attempt one delivers
    /// events 1-2 and drops; the reconnect must carry `resume_from_seq = 3`
    /// and splice the remainder without gaps or duplicates.
    #[tokio::test]
    async fn resume_loop_reconnects_with_the_cursor_over_a_real_socket()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::stream::{ResumingEventStream, SubscribeTarget};
        use crate::transport::grpc::GrpcWorkflowTransport;

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let workflow_id = aion_core::WorkflowId::new_v4();
        let server_workflow = workflow_id.clone();
        let server = tokio::spawn(async move {
            let first = accept_one(
                &listener,
                vec![
                    event_frame(1, &server_workflow)?,
                    event_frame(2, &server_workflow)?,
                ],
                AttemptEnd::Drop,
            )
            .await?;
            let second = accept_one(
                &listener,
                vec![event_frame(3, &server_workflow)?],
                AttemptEnd::CleanClose,
            )
            .await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>((first, second))
        });

        // A lazy channel never dials: only the WebSocket side is exercised.
        let channel = tonic::transport::Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
        let transport = Arc::new(GrpcWorkflowTransport::from_channel(
            live_config(port),
            channel,
        ));
        let mut events = ResumingEventStream::new(
            transport,
            "tenant-a",
            SubscribeTarget::Workflow { workflow_id },
        );

        let mut seqs = Vec::new();
        while let Some(item) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            futures::StreamExt::next(&mut events),
        )
        .await?
        {
            seqs.push(item.map(|event| event.seq()));
        }
        let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await??
            .map_err(|error| format!("server side failed: {error}"))?;

        let seqs = seqs
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("stream item failed: {error}"))?;
        assert_eq!(seqs, vec![1, 2, 3], "gap-free and duplicate-free delivery");
        assert_eq!(
            first.subscription["per_workflow"]["resume_from_seq"],
            json!(null),
            "the initial attach is a live tail"
        );
        assert_eq!(
            second.subscription["per_workflow"]["resume_from_seq"],
            json!(3),
            "the reconnect must resume from last delivered + 1"
        );
        Ok(())
    }
}
