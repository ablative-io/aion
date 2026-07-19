//! Protocol-faithful, real-route isolation coverage for console-style subscriptions.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use aion::{EngineBuilder, EventFilter, EventPublisher};
use aion_core::{Event, WorkflowId};
use aion_proto::StreamedEvent;
use aion_store::{EventStore, InMemoryStore};
use axum::http::StatusCode;
use futures::{SinkExt, StreamExt, stream, stream::BoxStream};
use serde_json::json;
use tokio::{
    net::TcpStream,
    sync::{Semaphore, broadcast},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message as ClientMessage, client::IntoClientRequest},
};

#[cfg(not(feature = "auth"))]
use super::test_support::TOKEN;
use super::{
    router::workflow_router,
    test_support::{NAMESPACE, runtime_config, server_state, started_event},
};
use crate::{
    NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces, config::NamespaceMode,
};

type ClientSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[tokio::test]
async fn two_console_style_sockets_remain_server_filtered_after_both_reconnect()
-> Result<(), Box<dyn std::error::Error>> {
    let first_workflow_id = WorkflowId::new(uuid::Uuid::from_u128(1));
    let second_workflow_id = WorkflowId::new(uuid::Uuid::from_u128(2));
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
    ownership.record(first_workflow_id.clone(), NAMESPACE)?;
    ownership.record(second_workflow_id.clone(), NAMESPACE)?;
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

    let (mut first_socket, mut second_socket) = connect_pair(address).await?;
    subscribe_pair(
        &mut first_socket,
        &mut second_socket,
        &first_workflow_id,
        &second_workflow_id,
        &publisher,
    )
    .await?;

    // Every publish is presented to both underlying broadcast subscribers. The
    // alternating order makes over-broad delivery fail deterministically on the
    // next read, rather than relying on a timeout to prove absence.
    publish_interleaved(&publisher, &second_workflow_id, &first_workflow_id, 1, 2)?;
    assert_next_event(&mut first_socket, &first_workflow_id, 1).await?;
    assert_next_event(&mut first_socket, &first_workflow_id, 2).await?;
    assert_next_event(&mut second_socket, &second_workflow_id, 1).await?;
    assert_next_event(&mut second_socket, &second_workflow_id, 2).await?;

    first_socket.send(ClientMessage::Close(None)).await?;
    second_socket.send(ClientMessage::Close(None)).await?;
    drop(first_socket);
    drop(second_socket);

    let (mut reconnected_first, mut reconnected_second) = connect_pair(address).await?;
    subscribe_pair(
        &mut reconnected_first,
        &mut reconnected_second,
        &first_workflow_id,
        &second_workflow_id,
        &publisher,
    )
    .await?;

    // Reverse which filter gets the first presented event after reconnect so
    // both fresh server-side subscriptions again encounter non-matching input.
    publish_interleaved(&publisher, &first_workflow_id, &second_workflow_id, 3, 4)?;
    assert_next_event(&mut reconnected_first, &first_workflow_id, 3).await?;
    assert_next_event(&mut reconnected_first, &first_workflow_id, 4).await?;
    assert_next_event(&mut reconnected_second, &second_workflow_id, 3).await?;
    assert_next_event(&mut reconnected_second, &second_workflow_id, 4).await?;

    server.abort();
    Ok(())
}

async fn connect_pair(
    address: SocketAddr,
) -> Result<(ClientSocket, ClientSocket), Box<dyn std::error::Error>> {
    let first = connect_event_socket(address).await?;
    let second = connect_event_socket(address).await?;
    Ok((first, second))
}

async fn connect_event_socket(
    address: SocketAddr,
) -> Result<ClientSocket, Box<dyn std::error::Error>> {
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
    let (socket, response) = connect_async(request).await?;
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    Ok(socket)
}

async fn subscribe_pair(
    first_socket: &mut ClientSocket,
    second_socket: &mut ClientSocket,
    first_workflow_id: &WorkflowId,
    second_workflow_id: &WorkflowId,
    publisher: &TestEventPublisher,
) -> Result<(), Box<dyn std::error::Error>> {
    send_console_subscription(first_socket, first_workflow_id).await?;
    send_console_subscription(second_socket, second_workflow_id).await?;
    publisher.wait_for_subscription().await;
    publisher.wait_for_subscription().await;
    Ok(())
}

async fn send_console_subscription(
    socket: &mut ClientSocket,
    workflow_id: &WorkflowId,
) -> Result<(), Box<dyn std::error::Error>> {
    let subscription = json!({
        "per_workflow": {
            "namespace": NAMESPACE,
            "workflow_id": workflow_id.to_string()
        }
    });
    socket
        .send(ClientMessage::Text(subscription.to_string().into()))
        .await?;
    Ok(())
}

fn publish_interleaved(
    publisher: &TestEventPublisher,
    first_presented: &WorkflowId,
    second_presented: &WorkflowId,
    first_sequence: u64,
    second_sequence: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    publisher.publish(event_for(first_presented.clone(), first_sequence)?)?;
    publisher.publish(event_for(second_presented.clone(), first_sequence)?)?;
    publisher.publish(event_for(first_presented.clone(), second_sequence)?)?;
    publisher.publish(event_for(second_presented.clone(), second_sequence)?)?;
    Ok(())
}

fn event_for(workflow_id: WorkflowId, sequence: u64) -> Result<Event, aion_core::PayloadError> {
    let mut event = started_event()?;
    let Event::WorkflowStarted { envelope, .. } = &mut event else {
        unreachable!("started_event fixture must remain WorkflowStarted");
    };
    envelope.workflow_id = workflow_id;
    envelope.seq = sequence;
    Ok(event)
}

async fn assert_next_event(
    socket: &mut ClientSocket,
    expected_workflow_id: &WorkflowId,
    expected_sequence: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(frame) = tokio::time::timeout(Duration::from_secs(2), socket.next()).await? else {
        return Err("websocket closed before the expected filtered event".into());
    };
    let ClientMessage::Text(text) = frame? else {
        return Err("expected websocket text frame".into());
    };
    let streamed: StreamedEvent = serde_json::from_str(&text)?;
    let event = streamed.decode_event()?;
    assert_eq!(event.workflow_id(), expected_workflow_id);
    assert_eq!(event.seq(), expected_sequence);
    Ok(())
}

struct TestEventPublisher {
    events: broadcast::Sender<Event>,
    subscribed: Semaphore,
}

impl TestEventPublisher {
    fn new() -> Self {
        let (events, _receiver) = broadcast::channel(16);
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
            .map_err(|error| error.to_string())
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
