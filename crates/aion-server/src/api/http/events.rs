//! WebSocket event-subscription handlers.

use axum::{
    extract::{
        State,
        ws::{WebSocket, WebSocketUpgrade},
    },
    response::{IntoResponse, Response},
};

use super::auth::WsCaller;
use crate::{CallerIdentity, ServerError, ServerState, stream::handle_subscription_socket};

pub(crate) async fn subscribe_events_socket(
    websocket: WebSocketUpgrade,
    State(state): State<ServerState>,
    WsCaller(caller): WsCaller,
) -> Response {
    websocket
        .on_upgrade(move |socket| async move {
            if let Err(error) = serve_subscription_socket(socket, state, caller).await {
                tracing::warn!(error = %error, "websocket event subscription ended with an error");
            }
        })
        .into_response()
}

async fn serve_subscription_socket(
    mut socket: WebSocket,
    state: ServerState,
    caller: CallerIdentity,
) -> Result<(), ServerError> {
    let request = match crate::api::ws_subscription::read_subscription_request(&mut socket).await {
        // A clean close before any subscribe frame (e.g. a StrictMode
        // double-mount tearing down its first socket) is a benign end, not an
        // error: return without an error frame or a logged warning.
        Ok(None) => return Ok(()),
        Ok(Some(request)) => request,
        Err(error) => {
            // Pre-stream rejections are sent as one terminal `{"error": ...}`
            // frame plus close, never a silent socket drop.
            crate::stream::socket::send_wire_error(&mut socket, &error.to_wire_error()).await?;
            return Err(error);
        }
    };
    // WS3: the cluster subscription is a NEW ARM of the single subscription
    // frame, not a workflow subscription. It is deployment-scoped (deploy-grant
    // authorized), reads supervisor/registry/store state rather than a namespace
    // engine, and carries its own snapshot + lag contract — so it is dispatched
    // to its own server before the workflow `subscribe_events` path. Every other
    // arm flows through the unchanged workflow path.
    if let Some(aion_proto::subscription_request::Subscription::Cluster(cluster)) =
        &request.subscription
    {
        let after_seq = cluster.after_seq;
        return crate::stream::serve_cluster_socket(socket, &state, &caller, after_seq).await;
    }
    handle_subscription_socket(socket, &state, &caller, &request).await
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::Arc};

    use aion::{EngineBuilder, EventFilter, EventPublisher};
    use aion_core::{Event, WorkflowId};
    use aion_proto::StreamedEvent;
    use aion_store::{EventStore, InMemoryStore};
    use axum::http::StatusCode;
    use futures::{SinkExt, StreamExt, stream, stream::BoxStream};
    use serde_json::json;
    use tokio::sync::{Semaphore, broadcast};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{Message as ClientMessage, client::IntoClientRequest},
    };

    use super::super::router::workflow_router;
    #[cfg(not(feature = "auth"))]
    use super::super::test_support::TOKEN;
    use super::super::test_support::{
        NAMESPACE, runtime_config, server_state, started_event, workflow_id,
    };
    use crate::{
        NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces,
        config::NamespaceMode,
    };

    #[tokio::test]
    async fn websocket_events_route_upgrades_and_streams_client_frame()
    -> Result<(), Box<dyn std::error::Error>> {
        let publisher = Arc::new(TestEventPublisher::new());
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(store)
                .in_memory_visibility()
                .scheduler_threads(1)
                .event_publisher(publisher.clone())
                .build()
                .await?,
        );
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(workflow_id(), NAMESPACE)?;
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            Arc::new(ownership),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        let router = workflow_router(server_state(resolver, runtime_config()).await?);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router.into_make_service()).await {
                tracing::warn!(%error, "test websocket server exited with error");
            }
        });

        let mut request = format!("ws://{address}/events/stream").into_client_request()?;
        #[cfg(feature = "auth")]
        let bearer = crate::auth::test_support::mint_token("alice", NAMESPACE)?;
        #[cfg(not(feature = "auth"))]
        let bearer = TOKEN.to_owned();
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {bearer}").parse()?);
        request
            .headers_mut()
            .insert("x-aion-subject", "alice".parse()?);
        request
            .headers_mut()
            .insert("x-aion-namespaces", NAMESPACE.parse()?);
        let (mut socket, response) = connect_async(request).await?;
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);

        let subscription = json!({
            "type": "subscribe",
            "subscription_id": "dashboard-test",
            "subscription": {
                "per_workflow": {
                    "namespace": NAMESPACE,
                    "workflow_id": workflow_id().to_string()
                }
            }
        });
        socket
            .send(ClientMessage::Text(subscription.to_string().into()))
            .await?;
        publisher.wait_for_subscription().await;
        publisher.publish(started_event()?)?;

        let Some(frame) = socket.next().await else {
            return Err("websocket closed before streaming an event".into());
        };
        let frame = frame?;
        let ClientMessage::Text(text) = frame else {
            return Err("expected websocket text frame".into());
        };
        let streamed: StreamedEvent = serde_json::from_str(&text)?;
        assert_eq!(streamed.namespace, NAMESPACE);
        assert_eq!(streamed.decode_event()?.workflow_id(), &workflow_id());

        server.abort();
        Ok(())
    }

    /// A browser cannot set `x-aion-namespaces` / `x-aion-subject` / the bearer
    /// `Authorization` header on a WebSocket handshake, so the same credentials
    /// must authorize the stream when supplied as query parameters. Without this
    /// the live feed is `namespace_denied` and reconnect-loops in every browser.
    #[tokio::test]
    async fn websocket_authorizes_via_query_params_when_headers_absent()
    -> Result<(), Box<dyn std::error::Error>> {
        let publisher = Arc::new(TestEventPublisher::new());
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(store)
                .in_memory_visibility()
                .scheduler_threads(1)
                .event_publisher(publisher.clone())
                .build()
                .await?,
        );
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(workflow_id(), NAMESPACE)?;
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            Arc::new(ownership),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        let router = workflow_router(server_state(resolver, runtime_config()).await?);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router.into_make_service()).await {
                tracing::warn!(%error, "test websocket server exited with error");
            }
        });

        #[cfg(feature = "auth")]
        let bearer = crate::auth::test_support::mint_token("alice", NAMESPACE)?;
        #[cfg(not(feature = "auth"))]
        let bearer = TOKEN.to_owned();
        // Credentials ride the query string; NO request headers are set — exactly
        // what a browser WebSocket can do.
        let url = format!(
            "ws://{address}/events/stream\
             ?x-aion-namespaces={NAMESPACE}&x-aion-subject=alice&access_token={bearer}"
        );
        let request = url.into_client_request()?;
        let (mut socket, response) = connect_async(request).await?;
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);

        let subscription = json!({
            "type": "subscribe",
            "subscription_id": "dashboard-test",
            "subscription": {
                "per_workflow": {
                    "namespace": NAMESPACE,
                    "workflow_id": workflow_id().to_string()
                }
            }
        });
        socket
            .send(ClientMessage::Text(subscription.to_string().into()))
            .await?;
        publisher.wait_for_subscription().await;
        publisher.publish(started_event()?)?;

        let Some(frame) = socket.next().await else {
            return Err("query-authorized websocket closed before streaming an event".into());
        };
        let ClientMessage::Text(text) = frame? else {
            return Err("expected websocket text frame".into());
        };
        let streamed: StreamedEvent = serde_json::from_str(&text)?;
        assert_eq!(streamed.namespace, NAMESPACE);
        assert_eq!(streamed.decode_event()?.workflow_id(), &workflow_id());

        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn websocket_subscription_rejections_send_one_terminal_error_frame_then_close()
    -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(store)
                .in_memory_visibility()
                .scheduler_threads(1)
                .build()
                .await?,
        );
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(workflow_id(), NAMESPACE)?;
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            Arc::new(ownership),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        let router = workflow_router(server_state(resolver, runtime_config()).await?);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router.into_make_service()).await {
                tracing::warn!(%error, "test websocket server exited with error");
            }
        });

        // Namespace-grant failure: the caller holds no grant for tenant-b.
        let denied_namespace = json!({
            "subscription": { "firehose": { "namespace": "tenant-b" } }
        });
        assert_terminal_ws_error(address, &denied_namespace, "namespace_denied").await?;

        // Workflow-level miss in a granted namespace: anti-existence-leak
        // NotFound, indistinguishable from a nonexistent workflow.
        let foreign_workflow = json!({
            "subscription": {
                "per_workflow": {
                    "namespace": NAMESPACE,
                    "workflow_id": WorkflowId::new(uuid::Uuid::from_u128(99)).to_string()
                }
            }
        });
        assert_terminal_ws_error(address, &foreign_workflow, "not_found").await?;

        server.abort();
        Ok(())
    }

    /// WS3 anti-leak: a cluster subscription from a caller WITHOUT the
    /// deployment-wide deploy grant is denied with exactly one terminal
    /// `namespace_denied` frame then close — never a snapshot, never topology.
    /// The standard event-stream credentials (namespace grant, no deploy claim)
    /// are sufficient to reach the gate and prove it denies.
    #[tokio::test]
    async fn cluster_subscription_without_deploy_grant_is_denied()
    -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(store)
                .in_memory_visibility()
                .scheduler_threads(1)
                .build()
                .await?,
        );
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(workflow_id(), NAMESPACE)?;
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            Arc::new(ownership),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        let router = workflow_router(server_state(resolver, runtime_config()).await?);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router.into_make_service()).await {
                tracing::warn!(%error, "test websocket server exited with error");
            }
        });

        let cluster = json!({ "subscription": { "cluster": { "after_seq": 0 } } });
        assert_terminal_ws_error(address, &cluster, "namespace_denied").await?;

        server.abort();
        Ok(())
    }

    async fn assert_terminal_ws_error(
        address: SocketAddr,
        subscription: &serde_json::Value,
        expected_code: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut request = format!("ws://{address}/events/stream").into_client_request()?;
        #[cfg(feature = "auth")]
        let bearer = crate::auth::test_support::mint_token("alice", NAMESPACE)?;
        #[cfg(not(feature = "auth"))]
        let bearer = TOKEN.to_owned();
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {bearer}").parse()?);
        request
            .headers_mut()
            .insert("x-aion-subject", "alice".parse()?);
        request
            .headers_mut()
            .insert("x-aion-namespaces", NAMESPACE.parse()?);
        let (mut socket, _response) = connect_async(request).await?;
        socket
            .send(ClientMessage::Text(subscription.to_string().into()))
            .await?;

        let Some(frame) = socket.next().await else {
            return Err("websocket closed without a terminal error frame".into());
        };
        let ClientMessage::Text(text) = frame? else {
            return Err("expected a text error frame before close".into());
        };
        let body: serde_json::Value = serde_json::from_str(&text)?;
        assert_eq!(
            body["error"]["code"],
            json!(expected_code),
            "terminal frame must wrap the wire error: {body}"
        );
        assert!(
            body["error"]["message"].is_string(),
            "terminal frame must carry the informational message: {body}"
        );

        loop {
            match socket.next().await {
                Some(Ok(ClientMessage::Close(_))) | None => return Ok(()),
                Some(Ok(ClientMessage::Ping(_) | ClientMessage::Pong(_))) => {}
                Some(Ok(other)) => {
                    return Err(
                        format!("expected close after the error frame, got {other:?}").into(),
                    );
                }
                Some(Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed)) => {
                    return Ok(());
                }
                Some(Err(error)) => return Err(error.into()),
            }
        }
    }

    struct TestEventPublisher {
        events: broadcast::Sender<Event>,
        subscribed: Semaphore,
    }

    impl TestEventPublisher {
        fn new() -> Self {
            let (events, _receiver) = broadcast::channel(8);
            Self {
                events,
                subscribed: Semaphore::new(0),
            }
        }

        async fn wait_for_subscription(&self) {
            if let Ok(permit) = self.subscribed.acquire().await {
                permit.forget();
            }
        }

        fn publish(&self, event: Event) -> Result<(), String> {
            self.events
                .send(event)
                .map(|_receivers| ())
                .map_err(|e| e.to_string())
        }
    }

    impl EventPublisher for TestEventPublisher {
        fn subscribe(
            &self,
            filter: EventFilter,
        ) -> BoxStream<'static, Result<Event, aion::EventStreamLagged>> {
            let receiver = self.events.subscribe();
            self.subscribed.add_permits(1);
            Box::pin(stream::unfold(
                (receiver, filter),
                |(mut receiver, filter)| async move {
                    loop {
                        match receiver.recv().await {
                            Ok(event) => {
                                if filter.matches(&event) {
                                    return Some((Ok(event), (receiver, filter)));
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                                return Some((
                                    Err(aion::EventStreamLagged { skipped }),
                                    (receiver, filter),
                                ));
                            }
                            Err(broadcast::error::RecvError::Closed) => return None,
                        }
                    }
                },
            ))
        }
    }
}
