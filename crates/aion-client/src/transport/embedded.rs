//! Transport backed by an in-process [`aion::Engine`].
//!
//! Event subscriptions honour the same resume/replay-splice contract as the
//! server's `/events/stream` endpoint, built directly on [`aion::Engine`]
//! seams (`Engine::subscribe` for the live broadcast, `engine.store()` for
//! the history snapshot — never a client-held stream over engine internals):
//!
//! 1. attach the live broadcast subscription FIRST (time T0);
//! 2. snapshot recorded history via `engine.store().read_history` (T1 > T0);
//! 3. validate the cursor against the snapshot head;
//! 4. splice: replay `[resume_from_seq ..= head]` from the snapshot, then the
//!    live tail filtered to `seq > head`.
//!
//! Gap-free: publish strictly follows durable commit, so every event with
//! `seq > head` was committed — and therefore broadcast — after T0.
//! Duplicate-free: the live filter drops every `seq <= head`, so an event
//! present in both the snapshot and the broadcast is emitted exactly once,
//! from the snapshot. Engine-side lag is never silent: each
//! `Err(EventStreamLagged)` item surfaces as `Err(ClientError::Unavailable)`
//! so the resume loop reconnects with its cursor.

use std::sync::Arc;

use aion_core::Event;
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{StreamExt, stream};

use crate::error::ClientError;
use crate::transport::contract::{SubscriptionAttempt, WorkflowTransport};

/// Transport backed by an in-process [`aion::Engine`].
pub struct EmbeddedWorkflowTransport {
    engine: Arc<aion::Engine>,
}

impl EmbeddedWorkflowTransport {
    /// Creates an embedded transport for `engine`.
    #[must_use]
    pub fn new(engine: Arc<aion::Engine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl WorkflowTransport for EmbeddedWorkflowTransport {
    async fn start_workflow(
        &self,
        request: aion_proto::ProtoStartWorkflowRequest,
    ) -> Result<aion_proto::ProtoStartWorkflowResponse, ClientError> {
        let input = request
            .input
            .ok_or_else(|| ClientError::invalid_argument("start request input payload is missing"))
            .and_then(|payload| {
                aion_core::Payload::try_from(payload).map_err(ClientError::from_wire_error)
            })?;
        // The embedded engine is single-tenant and in-process: there is no
        // namespace authority stamping visibility attributes, so the start
        // carries no search attributes.
        let handle = self
            .engine
            .start_workflow(
                &request.workflow_type,
                input,
                std::collections::HashMap::new(),
                String::from("default"),
            )
            .await
            .map_err(|error| map_engine_error(&error))?;
        Ok(aion_proto::ProtoStartWorkflowResponse {
            workflow_id: Some(aion_proto::ProtoWorkflowId::from(
                handle.workflow_id().clone(),
            )),
            run_id: Some(aion_proto::ProtoRunId::from(handle.run_id().clone())),
        })
    }

    async fn signal(
        &self,
        request: aion_proto::ProtoSignalRequest,
    ) -> Result<aion_proto::ProtoSignalResponse, ClientError> {
        let workflow_id = decode_required_workflow_id(request.workflow_id)?;
        let run_id = decode_required_run_id(request.run_id)?;
        let payload = request
            .payload
            .ok_or_else(|| ClientError::invalid_argument("signal request payload is missing"))
            .and_then(|payload| {
                aion_core::Payload::try_from(payload).map_err(ClientError::from_wire_error)
            })?;
        self.engine
            .signal(&workflow_id, &run_id, request.signal_name, payload)
            .await
            .map_err(|error| map_engine_error(&error))?;
        Ok(aion_proto::ProtoSignalResponse {})
    }

    async fn query(
        &self,
        request: aion_proto::ProtoQueryRequest,
    ) -> Result<aion_proto::ProtoQueryResponse, ClientError> {
        let workflow_id = decode_required_workflow_id(request.workflow_id)?;
        let run_id = decode_required_run_id(request.run_id)?;
        let payload = self
            .engine
            .query(&workflow_id, &run_id, request.query_name)
            .await
            .map_err(|error| map_engine_error(&error))?;
        Ok(aion_proto::ProtoQueryResponse {
            outcome: Some(aion_proto::proto_query_response::Outcome::Result(
                aion_proto::ProtoPayload::from(payload),
            )),
        })
    }

    async fn cancel(
        &self,
        request: aion_proto::ProtoCancelRequest,
    ) -> Result<aion_proto::ProtoCancelResponse, ClientError> {
        let workflow_id = decode_required_workflow_id(request.workflow_id)?;
        let run_id = decode_required_run_id(request.run_id)?;
        self.engine
            .cancel(&workflow_id, &run_id, request.reason)
            .await
            .map_err(|error| map_engine_error(&error))?;
        Ok(aion_proto::ProtoCancelResponse {})
    }

    async fn list_workflows(
        &self,
        request: aion_proto::ProtoListWorkflowsRequest,
    ) -> Result<aion_proto::ProtoListWorkflowsResponse, ClientError> {
        let filter = match request.filter.as_ref() {
            Some(filter) => {
                aion_proto::decode_workflow_filter(filter).map_err(ClientError::from_wire_error)?
            }
            None => aion_core::WorkflowFilter::default(),
        };
        let summaries = self
            .engine
            .list_workflows(filter)
            .await
            .map_err(|error| map_engine_error(&error))?
            .iter()
            .map(|summary| {
                aion_proto::encode_workflow_summary(request.namespace.clone(), None, summary)
            })
            .map(|result| result.map_err(ClientError::from_wire_error))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(aion_proto::ProtoListWorkflowsResponse { summaries })
    }

    async fn describe_workflow(
        &self,
        request: aion_proto::ProtoDescribeWorkflowRequest,
    ) -> Result<aion_proto::ProtoDescribeWorkflowResponse, ClientError> {
        let workflow_id = decode_required_workflow_id(request.workflow_id)?;
        let history = self
            .engine
            .store()
            .read_history(&workflow_id)
            .await
            .map_err(|error| ClientError::server(error.to_string()))?;
        let Some(summary) = aion_core::WorkflowSummary::from_history(&history) else {
            return Err(ClientError::not_found(format!(
                "workflow {workflow_id} has no recorded history"
            )));
        };
        let summary = Some(
            aion_proto::encode_workflow_summary(request.namespace.clone(), None, &summary)
                .map_err(ClientError::from_wire_error)?,
        );
        let history = if request.include_history {
            history
                .iter()
                .map(|event| aion_proto::encode_event(request.namespace.clone(), None, event))
                .map(|result| result.map_err(ClientError::from_wire_error))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        Ok(aion_proto::ProtoDescribeWorkflowResponse { summary, history })
    }

    async fn subscribe(
        &self,
        request: aion_proto::SubscriptionRequest,
        resume_from_sequence: Option<u64>,
    ) -> Result<SubscriptionAttempt, ClientError> {
        let (workflow_target, filter) = embedded_subscription_target(request)?;
        // T0: attach to the live broadcast BEFORE any history snapshot — one
        // half of the gap-free splice proof (mirrors the server's
        // subscribe-then-snapshot ordering).
        let live = self.engine.subscribe(filter);
        let events = match (&workflow_target, resume_from_sequence) {
            (Some(workflow_id), Some(resume_from_seq)) => {
                // T1 (> T0): snapshot recorded history, then validate the
                // cursor against its head and build the dedupe splice.
                let history = self
                    .engine
                    .store()
                    .read_history(workflow_id)
                    .await
                    .map_err(|error| ClientError::server(error.to_string()))?;
                splice_resume(live, history, resume_from_seq)?
            }
            (None, Some(_)) => {
                return Err(ClientError::invalid_argument(
                    "filtered and firehose event streams are live-only by design; resume \
                     cursors are valid for per-workflow subscriptions only",
                ));
            }
            (_, None) => map_lag(live),
        };
        // Per-workflow streams end at the run's terminal event, exactly like
        // the server socket; callers walk continue-as-new chains by
        // resubscribing with their cursor.
        Ok(SubscriptionAttempt::new(match workflow_target {
            Some(_) => close_after_terminal(events),
            None => events,
        }))
    }
}

/// Validates a resume cursor against a history snapshot and builds the
/// replay/live splice (see the module docs for the gap/duplicate proof).
fn splice_resume(
    live: BoxStream<'static, Result<Event, aion::EventStreamLagged>>,
    history: Vec<Event>,
    resume_from_seq: u64,
) -> Result<BoxStream<'static, Result<Event, ClientError>>, ClientError> {
    if resume_from_seq == 0 {
        return Err(ClientError::invalid_argument(
            "resume_from_seq must be >= 1 (the first sequence number wanted)",
        ));
    }
    let head = history.last().map_or(0, Event::seq);
    if resume_from_seq > head.saturating_add(1) {
        return Err(ClientError::invalid_argument(format!(
            "resume_from_seq {resume_from_seq} is ahead of recorded history (head seq {head}); \
             the largest valid cursor is {}",
            head.saturating_add(1)
        )));
    }

    let mut history = history;
    let replay_start = history.partition_point(|event| event.seq() < resume_from_seq);
    let replay = history.split_off(replay_start);
    let tail = live.filter(move |item| {
        let keep = match item {
            Ok(event) => event.seq() > head,
            // Lag is information, never filtered away.
            Err(aion::EventStreamLagged { .. }) => true,
        };
        futures::future::ready(keep)
    });

    Ok(stream::iter(replay.into_iter().map(Ok))
        .chain(map_lag(tail.boxed()))
        .boxed())
}

/// Maps engine-side lag items to retryable [`ClientError::Unavailable`] so
/// the resume loop reconnects with its cursor instead of silently gapping.
fn map_lag(
    live: BoxStream<'static, Result<Event, aion::EventStreamLagged>>,
) -> BoxStream<'static, Result<Event, ClientError>> {
    live.map(|item| {
        item.map_err(|lagged| {
            ClientError::from_wire_error(aion_proto::WireError::lagged(lagged.to_string()))
        })
    })
    .boxed()
}

/// Ends the stream after the first terminal workflow event, mirroring the
/// server socket's per-workflow run-boundary close.
fn close_after_terminal(
    events: BoxStream<'static, Result<Event, ClientError>>,
) -> BoxStream<'static, Result<Event, ClientError>> {
    stream::unfold(Some(events), |state| async move {
        let mut events = state?;
        let item = events.next().await?;
        // The terminal event is delivered and the inner stream is dropped
        // immediately afterwards (releasing the broadcast receiver), so the
        // close is eager — it never waits for a further event to be polled.
        let closed = matches!(&item, Ok(event) if is_terminal_workflow_event(event));
        Some((item, if closed { None } else { Some(events) }))
    })
    .boxed()
}

fn is_terminal_workflow_event(event: &Event) -> bool {
    matches!(
        event,
        Event::WorkflowCompleted { .. }
            | Event::WorkflowFailed { .. }
            | Event::WorkflowCancelled { .. }
            | Event::WorkflowTimedOut { .. }
            | Event::WorkflowContinuedAsNew { .. }
    )
}

fn decode_required_workflow_id(
    value: Option<aion_proto::ProtoWorkflowId>,
) -> Result<aion_core::WorkflowId, ClientError> {
    value
        .ok_or_else(|| ClientError::invalid_argument("request workflow id is missing"))?
        .try_into()
        .map_err(ClientError::from_wire_error)
}

fn decode_required_run_id(
    value: Option<aion_proto::ProtoRunId>,
) -> Result<aion_core::RunId, ClientError> {
    value
        .ok_or_else(|| ClientError::invalid_argument("request run id is missing"))?
        .try_into()
        .map_err(ClientError::from_wire_error)
}

/// Maps a wire subscription request onto the engine filter surface plus the
/// per-workflow target the splice and run-boundary close key on.
fn embedded_subscription_target(
    request: aion_proto::SubscriptionRequest,
) -> Result<(Option<aion_core::WorkflowId>, aion::EventFilter), ClientError> {
    match request.subscription {
        Some(aion_proto::subscription_request::Subscription::PerWorkflow(subscription)) => {
            let workflow_id = subscription
                .workflow_id
                .ok_or_else(|| {
                    ClientError::invalid_argument(
                        "per-workflow subscription requires a workflow id",
                    )
                })?
                .try_into()
                .map_err(ClientError::from_wire_error)?;
            Ok((
                Some(aion_core::WorkflowId::clone(&workflow_id)),
                aion::EventFilter {
                    workflow_id: Some(workflow_id),
                    run: None,
                    family: None,
                },
            ))
        }
        Some(
            aion_proto::subscription_request::Subscription::Filtered(_)
            | aion_proto::subscription_request::Subscription::Firehose(_),
        ) => Ok((None, aion::EventFilter::default())),
        None => Err(ClientError::invalid_argument(
            "subscription request is missing its subscription variant",
        )),
    }
}

fn map_engine_error(error: &aion::EngineError) -> ClientError {
    match error {
        aion::EngineError::WorkflowNotFound { .. } => ClientError::not_found(error.to_string()),
        aion::EngineError::ShuttingDown => ClientError::unavailable(error.to_string()),
        _ => ClientError::server(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::time::Duration;

    use aion::EventStreamLagged;
    use aion_core::{Event, EventEnvelope, Payload, RunId, WorkflowId};
    use chrono::Utc;
    use futures::{StreamExt, stream};

    use super::{close_after_terminal, map_lag, splice_resume};
    use crate::error::ClientError;

    fn workflow_id() -> WorkflowId {
        WorkflowId::new(uuid::Uuid::from_u128(1))
    }

    fn envelope(seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: Utc::now(),
            workflow_id: workflow_id(),
        }
    }

    fn signal(seq: u64) -> Result<Event, aion_core::PayloadError> {
        Ok(Event::SignalReceived {
            envelope: envelope(seq),
            name: format!("signal-{seq}"),
            payload: Payload::from_json(&serde_json::json!({ "seq": seq }))?,
        })
    }

    fn completed(seq: u64) -> Result<Event, aion_core::PayloadError> {
        Ok(Event::WorkflowCompleted {
            envelope: envelope(seq),
            result: Payload::from_json(&serde_json::json!({ "seq": seq }))?,
        })
    }

    fn history(seqs: std::ops::RangeInclusive<u64>) -> Result<Vec<Event>, aion_core::PayloadError> {
        seqs.map(signal).collect()
    }

    fn live(
        items: Vec<Result<Event, EventStreamLagged>>,
    ) -> futures::stream::BoxStream<'static, Result<Event, EventStreamLagged>> {
        stream::iter(items).boxed()
    }

    async fn delivered_seqs(
        events: futures::stream::BoxStream<'static, Result<Event, ClientError>>,
    ) -> Result<Vec<u64>, ClientError> {
        events
            .map(|item| item.map(|event| event.seq()))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect()
    }

    #[tokio::test]
    async fn cursor_zero_is_invalid_argument() -> Result<(), Box<dyn std::error::Error>> {
        let error = splice_resume(live(Vec::new()), history(1..=3)?, 0).err();

        let Some(ClientError::InvalidArgument { detail }) = error else {
            return Err(format!("cursor 0 must be InvalidArgument, got {error:?}").into());
        };
        assert!(detail.message.contains(">= 1"), "detail: {detail}");
        Ok(())
    }

    #[tokio::test]
    async fn cursor_ahead_of_history_is_invalid_argument() -> Result<(), Box<dyn std::error::Error>>
    {
        let error = splice_resume(live(Vec::new()), history(1..=5)?, 7).err();

        let Some(ClientError::InvalidArgument { detail }) = error else {
            return Err(format!("cursor head+2 must be InvalidArgument, got {error:?}").into());
        };
        assert!(
            detail.message.contains("ahead of recorded history"),
            "{detail}"
        );

        let empty = splice_resume(live(Vec::new()), Vec::new(), 2).err();
        assert!(
            matches!(empty, Some(ClientError::InvalidArgument { .. })),
            "cursor 2 over empty history must be rejected, got {empty:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn overlap_between_snapshot_and_live_is_deduplicated_contiguous_unique()
    -> Result<(), Box<dyn std::error::Error>> {
        // Snapshot holds 1..=5; the live broadcast re-emits 4 and 5 (arrived
        // between attach and snapshot) before the genuinely new 6.
        let events = splice_resume(
            live(vec![Ok(signal(4)?), Ok(signal(5)?), Ok(signal(6)?)]),
            history(1..=5)?,
            1,
        )?;

        assert_eq!(delivered_seqs(events).await?, vec![1, 2, 3, 4, 5, 6]);
        Ok(())
    }

    #[tokio::test]
    async fn mid_history_cursor_replays_suffix_only() -> Result<(), Box<dyn std::error::Error>> {
        let events = splice_resume(live(vec![Ok(signal(6)?)]), history(1..=5)?, 3)?;

        assert_eq!(delivered_seqs(events).await?, vec![3, 4, 5, 6]);
        Ok(())
    }

    #[tokio::test]
    async fn cursor_at_head_plus_one_yields_empty_replay_and_live_tail_only()
    -> Result<(), Box<dyn std::error::Error>> {
        let events = splice_resume(
            live(vec![Ok(signal(6)?), Ok(signal(7)?)]),
            history(1..=5)?,
            6,
        )?;

        assert_eq!(delivered_seqs(events).await?, vec![6, 7]);
        Ok(())
    }

    #[tokio::test]
    async fn lag_mid_splice_surfaces_unavailable_after_the_replay()
    -> Result<(), Box<dyn std::error::Error>> {
        let events = splice_resume(
            live(vec![Err(EventStreamLagged { skipped: 3 })]),
            history(1..=2)?,
            1,
        )?;
        let collected: Vec<_> = events.collect().await;

        assert_eq!(collected.len(), 3, "two replay events then the lag item");
        assert!(collected[0].is_ok() && collected[1].is_ok());
        assert!(
            matches!(
                collected[2].as_ref().err(),
                Some(ClientError::Unavailable { .. })
            ),
            "lag must surface as retryable Unavailable, never a silent gap, got {:?}",
            collected[2]
        );
        Ok(())
    }

    #[tokio::test]
    async fn per_workflow_stream_closes_after_terminal_event()
    -> Result<(), Box<dyn std::error::Error>> {
        // Terminal at seq 3 mid-replay: deliver 1..=3 and close without
        // draining the live tail (continue-as-new/terminal run boundary).
        let mut history = history(1..=2)?;
        history.push(completed(3)?);
        history.push(signal(4)?);
        let events = splice_resume(live(vec![Ok(signal(5)?)]), history, 1)?;

        assert_eq!(
            delivered_seqs(close_after_terminal(events)).await?,
            vec![1, 2, 3],
            "the stream must close after the terminal event"
        );
        Ok(())
    }

    #[tokio::test]
    async fn live_lag_maps_to_unavailable() -> Result<(), Box<dyn std::error::Error>> {
        let events = map_lag(live(vec![
            Ok(signal(1)?),
            Err(EventStreamLagged { skipped: 9 }),
        ]));
        let collected: Vec<_> = events.collect().await;

        assert_eq!(collected.len(), 2);
        assert!(
            matches!(
                collected[1].as_ref().err(),
                Some(ClientError::Unavailable { .. })
            ),
            "got {:?}",
            collected[1]
        );
        Ok(())
    }

    /// End-to-end through a real engine: the embedded resume splice delivers
    /// recorded history and live appends gap-free and duplicate-free, built
    /// on `Engine::subscribe` + `engine.store()` (the pin-note seams).
    #[tokio::test]
    async fn embedded_resume_splices_recorded_history_with_live_appends()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::stream::SubscribeTarget;
        use crate::transport::{EmbeddedWorkflowTransport, WorkflowTransport};

        let capacity = NonZeroUsize::new(16).ok_or("capacity must be non-zero")?;
        let engine = std::sync::Arc::new(
            aion::EngineBuilder::new()
                .store(aion_store::InMemoryStore::default())
                .in_memory_visibility()
                .event_streaming(capacity)
                .build()
                .await?,
        );
        let workflow_id = WorkflowId::new_v4();
        let mut recorder = aion::durability::Recorder::new(workflow_id.clone(), engine.store());
        recorder
            .record_workflow_started(
                Utc::now(),
                aion::durability::WorkflowStartRecord {
                    workflow_type: String::from("checkout"),
                    input: Payload::from_json(&serde_json::json!({ "cart": [] }))?,
                    run_id: RunId::new(uuid::Uuid::from_u128(7)),
                    parent_run_id: None,
                    package_version: aion_core::PackageVersion::new("a".repeat(64)),
                },
            )
            .await?;
        for seq in 2..=3 {
            recorder
                .record_signal_received(
                    Utc::now(),
                    format!("signal-{seq}"),
                    Payload::from_json(&serde_json::json!({ "seq": seq }))?,
                )
                .await?;
        }

        // Resume from seq 2: replay [2, 3] from the snapshot, then splice the
        // live append (4) with no gaps and no duplicates.
        let transport = EmbeddedWorkflowTransport::new(std::sync::Arc::clone(&engine));
        let request = SubscribeTarget::Workflow {
            workflow_id: workflow_id.clone(),
        }
        .request("default");
        let attempt = transport.subscribe(request, Some(2)).await?;
        let mut events = attempt.events;

        let mut delivered = Vec::new();
        for _ in 0..2 {
            let item = tokio::time::timeout(Duration::from_secs(2), events.next())
                .await
                .map_err(|_| "timed out waiting for a replay event")?
                .ok_or("stream ended before the replay completed")?;
            delivered.push(item?.seq());
        }
        recorder
            .record_workflow_completed(
                Utc::now(),
                Payload::from_json(&serde_json::json!({ "done": true }))?,
            )
            .await?;
        let item = tokio::time::timeout(Duration::from_secs(2), events.next())
            .await
            .map_err(|_| "timed out waiting for the live spliced event")?
            .ok_or("stream ended before the live event arrived")?;
        delivered.push(item?.seq());
        assert_eq!(delivered, vec![2, 3, 4]);

        // Seq 4 is terminal: the per-workflow stream must now close.
        let end = tokio::time::timeout(Duration::from_secs(2), events.next())
            .await
            .map_err(|_| "timed out waiting for the post-terminal close")?;
        assert!(
            end.is_none(),
            "per-workflow stream must close after the terminal event, got {end:?}"
        );

        // A cursor beyond head + 1 is rejected against the same engine.
        let ahead = transport
            .subscribe(
                SubscribeTarget::Workflow { workflow_id }.request("default"),
                Some(9),
            )
            .await
            .err();
        assert!(
            matches!(ahead, Some(ClientError::InvalidArgument { .. })),
            "cursor ahead of history must be InvalidArgument, got {ahead:?}"
        );

        engine.shutdown()?;
        Ok(())
    }
}
