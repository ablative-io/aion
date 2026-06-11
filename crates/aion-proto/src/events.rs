//! Event-streaming wire types.

use crate::convert::{
    ProtoWorkflowId, ProtoWorkflowStatus, WireEnvelope, decode_core_value, encode_core_value,
};
use crate::error::WireError;

/// Proto representation of an event subscription request.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct SubscriptionRequest {
    /// Requested subscription model.
    #[prost(oneof = "subscription_request::Subscription", tags = "1, 2, 3")]
    pub subscription: Option<subscription_request::Subscription>,
}

/// Types nested under [`SubscriptionRequest`].
pub mod subscription_request {
    /// Proto oneof for the available subscription models.
    #[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Oneof)]
    pub enum Subscription {
        /// Events for a single workflow in the caller's namespace.
        #[prost(message, tag = "1")]
        PerWorkflow(super::PerWorkflowSubscription),
        /// Events matching optional selectors scoped by the caller's namespace.
        #[prost(message, tag = "2")]
        Filtered(super::FilteredSubscription),
        /// All events visible in the caller's namespace.
        #[prost(message, tag = "3")]
        Firehose(super::FirehoseSubscription),
    }
}

/// Subscribe to events for one workflow.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct PerWorkflowSubscription {
    /// Caller namespace used for adapter-boundary authorisation.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Workflow whose events are requested.
    #[prost(message, optional, tag = "2")]
    pub workflow_id: Option<ProtoWorkflowId>,
    /// First per-workflow sequence number the caller wants — not the last seq
    /// already seen. When present, the server replays recorded history events
    /// with seq >= `resume_from_seq` in order, then splices into the live
    /// stream with no gaps and no duplicates. Sequence numbers start at 1; 0
    /// is rejected as `invalid_input`. Absent = live tail only (current
    /// behaviour). `resume_from_seq` = 1 replays the full history.
    ///
    /// Only per-workflow subscriptions carry a resume cursor: per-workflow
    /// seq is the only ordering that exists, so [`FilteredSubscription`] and
    /// [`FirehoseSubscription`] are live-only by design.
    ///
    /// RESERVED compaction signal (documentation-only, no code yet): a cursor
    /// older than the earliest retained event yields `not_found` with
    /// `error_type` `"HistoryCompacted"`; callers restart with a fresh
    /// subscription.
    #[prost(uint64, optional, tag = "3")]
    pub resume_from_seq: Option<u64>,
}

/// Subscribe to events selected by optional workflow metadata.
///
/// Filtered streams carry NO resume cursor and are live-only by design:
/// per-workflow seq is the only ordering that exists, so resumption is not
/// representable here. Disconnection after at least one delivered event
/// surfaces Unavailable client-side — never a silent gapped reattach.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct FilteredSubscription {
    /// Caller namespace used for adapter-boundary authorisation.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Optional workflow type selector.
    #[prost(string, optional, tag = "2")]
    pub workflow_type: Option<String>,
    /// Optional workflow status selector.
    #[prost(enumeration = "ProtoWorkflowStatus", optional, tag = "3")]
    pub status: Option<i32>,
    /// Optional namespace selector distinct from the caller namespace.
    #[prost(string, optional, tag = "4")]
    pub namespace_selector: Option<String>,
}

/// Subscribe to every event visible in the caller's namespace.
///
/// Firehose streams carry NO resume cursor and are live-only by design:
/// per-workflow seq is the only ordering that exists, so resumption is not
/// representable here. Disconnection after at least one delivered event
/// surfaces Unavailable client-side — never a silent gapped reattach.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct FirehoseSubscription {
    /// Caller namespace used for adapter-boundary authorisation.
    #[prost(string, tag = "1")]
    pub namespace: String,
}

/// Streamed event frame carrying an unmodified aion-core `Event` in a wire envelope.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct StreamedEvent {
    /// Namespace that owns the event.
    #[prost(string, tag = "1")]
    pub namespace: String,
    /// Serde-encoded aion-core `Event` envelope.
    #[prost(message, optional, tag = "2")]
    pub event: Option<WireEnvelope>,
}

impl StreamedEvent {
    /// Serializes an aion-core event into a streamed event frame.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] with code `backend` if the event cannot be
    /// serialized into the shared core-value envelope.
    pub fn encode(
        namespace: impl Into<String>,
        request_id: Option<String>,
        event: &aion_core::Event,
    ) -> Result<Self, WireError> {
        let namespace = namespace.into();
        let event = encode_core_value(namespace.clone(), request_id, event)?;
        Ok(Self {
            namespace,
            event: Some(event),
        })
    }

    /// Decodes the enclosed aion-core event after checking namespace consistency.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] with code `backend` if the frame is missing its
    /// event envelope, if the frame namespace differs from the envelope
    /// namespace, or if the core event cannot be decoded.
    pub fn decode_event(&self) -> Result<aion_core::Event, WireError> {
        let event = self
            .event
            .as_ref()
            .ok_or_else(|| WireError::backend("streamed event envelope is missing"))?;
        if event.namespace != self.namespace {
            return Err(WireError::backend("streamed event namespace mismatch"));
        }
        decode_core_value(event)
    }
}

/// Serializes an aion-core event into a streamed event frame.
///
/// # Errors
///
/// Returns [`WireError`] with code `backend` if the event cannot be serialized.
pub fn encode_streamed_event(
    namespace: impl Into<String>,
    request_id: Option<String>,
    event: &aion_core::Event,
) -> Result<StreamedEvent, WireError> {
    StreamedEvent::encode(namespace, request_id, event)
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use prost::Message;
    use serde_json::json;

    use super::{
        FilteredSubscription, FirehoseSubscription, PerWorkflowSubscription, StreamedEvent,
        SubscriptionRequest, encode_streamed_event, subscription_request,
    };
    use crate::convert::{ProtoWorkflowId, ProtoWorkflowStatus, WireEnvelope};
    use crate::error::WireError;

    fn workflow_id() -> aion_core::WorkflowId {
        aion_core::WorkflowId::new(uuid::Uuid::nil())
    }

    fn recorded_at() -> Result<DateTime<Utc>, chrono::ParseError> {
        Ok(DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")?.with_timezone(&Utc))
    }

    fn event_envelope() -> Result<aion_core::EventEnvelope, chrono::ParseError> {
        Ok(aion_core::EventEnvelope {
            seq: 1,
            recorded_at: recorded_at()?,
            workflow_id: workflow_id(),
        })
    }

    #[test]
    fn subscription_request_round_trips_all_variants() -> Result<(), Box<dyn std::error::Error>> {
        let requests = [
            SubscriptionRequest {
                subscription: Some(subscription_request::Subscription::PerWorkflow(
                    PerWorkflowSubscription {
                        namespace: String::from("tenant-a"),
                        workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
                        resume_from_seq: None,
                    },
                )),
            },
            SubscriptionRequest {
                subscription: Some(subscription_request::Subscription::PerWorkflow(
                    PerWorkflowSubscription {
                        namespace: String::from("tenant-a"),
                        workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
                        resume_from_seq: Some(42),
                    },
                )),
            },
            SubscriptionRequest {
                subscription: Some(subscription_request::Subscription::Filtered(
                    FilteredSubscription {
                        namespace: String::from("tenant-a"),
                        workflow_type: Some(String::from("checkout")),
                        status: Some(ProtoWorkflowStatus::Running as i32),
                        namespace_selector: Some(String::from("tenant-a")),
                    },
                )),
            },
            SubscriptionRequest {
                subscription: Some(subscription_request::Subscription::Filtered(
                    FilteredSubscription {
                        namespace: String::from("tenant-a"),
                        workflow_type: None,
                        status: None,
                        namespace_selector: None,
                    },
                )),
            },
            SubscriptionRequest {
                subscription: Some(subscription_request::Subscription::Firehose(
                    FirehoseSubscription {
                        namespace: String::from("tenant-a"),
                    },
                )),
            },
        ];

        for request in requests {
            let json = serde_json::to_vec(&request)?;
            let from_json: SubscriptionRequest = serde_json::from_slice(&json)?;
            assert_eq!(from_json, request);

            let bytes = request.encode_to_vec();
            let from_proto = SubscriptionRequest::decode(bytes.as_slice())?;
            assert_eq!(from_proto, request);
        }

        Ok(())
    }

    #[test]
    fn per_workflow_resume_cursor_round_trips_prost() -> Result<(), Box<dyn std::error::Error>> {
        let with_cursor = PerWorkflowSubscription {
            namespace: String::from("tenant-a"),
            workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
            resume_from_seq: Some(7),
        };
        let decoded = PerWorkflowSubscription::decode(with_cursor.encode_to_vec().as_slice())?;
        assert_eq!(decoded, with_cursor);
        assert_eq!(decoded.resume_from_seq, Some(7));

        let without_cursor = PerWorkflowSubscription {
            namespace: String::from("tenant-a"),
            workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
            resume_from_seq: None,
        };
        let decoded = PerWorkflowSubscription::decode(without_cursor.encode_to_vec().as_slice())?;
        assert_eq!(decoded, without_cursor);
        assert_eq!(decoded.resume_from_seq, None);

        Ok(())
    }

    #[test]
    fn per_workflow_resume_cursor_json_shape_is_pinned() -> Result<(), Box<dyn std::error::Error>> {
        let with_cursor = PerWorkflowSubscription {
            namespace: String::from("tenant-a"),
            workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
            resume_from_seq: Some(7),
        };
        let value = serde_json::to_value(&with_cursor)?;
        assert_eq!(
            value,
            json!({
                "namespace": "tenant-a",
                "workflow_id": { "uuid": "00000000-0000-0000-0000-000000000000" },
                "resume_from_seq": 7,
            })
        );
        let from_json: PerWorkflowSubscription = serde_json::from_value(value)?;
        assert_eq!(from_json, with_cursor);

        let without_cursor = PerWorkflowSubscription {
            namespace: String::from("tenant-a"),
            workflow_id: Some(ProtoWorkflowId::from(workflow_id())),
            resume_from_seq: None,
        };
        let value = serde_json::to_value(&without_cursor)?;
        assert_eq!(
            value,
            json!({
                "namespace": "tenant-a",
                "workflow_id": { "uuid": "00000000-0000-0000-0000-000000000000" },
                "resume_from_seq": null,
            })
        );
        let from_json: PerWorkflowSubscription = serde_json::from_value(value)?;
        assert_eq!(from_json, without_cursor);

        Ok(())
    }

    #[test]
    fn subscription_request_without_resume_field_decodes_to_none()
    -> Result<(), Box<dyn std::error::Error>> {
        let request: SubscriptionRequest = serde_json::from_value(json!({
            "subscription": {
                "PerWorkflow": {
                    "namespace": "tenant-a",
                    "workflow_id": { "uuid": "00000000-0000-0000-0000-000000000000" },
                }
            }
        }))?;

        let Some(subscription_request::Subscription::PerWorkflow(per_workflow)) =
            request.subscription
        else {
            return Err(Box::from("expected a per-workflow subscription"));
        };
        assert_eq!(per_workflow.namespace, "tenant-a");
        assert_eq!(
            per_workflow.workflow_id,
            Some(ProtoWorkflowId::from(workflow_id()))
        );
        assert_eq!(per_workflow.resume_from_seq, None);

        Ok(())
    }

    #[test]
    fn streamed_event_round_trips_core_event() -> Result<(), Box<dyn std::error::Error>> {
        let event = aion_core::Event::WorkflowStarted {
            envelope: event_envelope()?,
            workflow_type: String::from("checkout"),
            input: aion_core::Payload::from_json(&json!({ "cart": ["sku-1"] }))?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        };

        let frame = encode_streamed_event("tenant-a", Some(String::from("request-1")), &event)?;
        assert_eq!(frame.namespace, "tenant-a");
        let envelope = frame
            .event
            .as_ref()
            .ok_or_else(|| WireError::backend("test streamed event envelope is missing"))?;
        assert_eq!(envelope.namespace, "tenant-a");
        assert_eq!(envelope.request_id.as_deref(), Some("request-1"));

        let decoded = frame.decode_event()?;
        assert_eq!(decoded, event);
        Ok(())
    }

    #[test]
    fn streamed_event_rejects_namespace_mismatch() {
        let frame = StreamedEvent {
            namespace: String::from("tenant-a"),
            event: Some(WireEnvelope {
                namespace: String::from("tenant-b"),
                request_id: None,
                payload: None,
            }),
        };

        assert_eq!(
            frame.decode_event(),
            Err(WireError::backend("streamed event namespace mismatch"))
        );
    }

    #[test]
    fn streamed_event_rejects_missing_envelope() {
        let frame = StreamedEvent {
            namespace: String::from("tenant-a"),
            event: None,
        };

        assert_eq!(
            frame.decode_event(),
            Err(WireError::backend("streamed event envelope is missing"))
        );
    }
}
