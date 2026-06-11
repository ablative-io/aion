//! `SubscriptionRequest` to `EventFilter` mapping.

use aion::EventFilter;
use aion_core::WorkflowId;
use aion_proto::{ProtoWorkflowId, ProtoWorkflowStatus, SubscriptionRequest, subscription_request};
use futures::stream::BoxStream;

use crate::error::ServerError;
use crate::namespace::{
    CallerIdentity, NamespaceGuard, NamespaceOperation, SubscriptionScope, WorkflowTarget,
};
use crate::stream::selector::SubscriptionSelector;

/// Authorized subscription returned by the adapter boundary.
pub struct EventSubscription {
    /// Namespace authorized by the guard.
    pub namespace: String,
    /// Engine-side filter used for the subscription.
    pub filter: EventFilter,
    /// Workflow-type/status selectors applied server-side before encoding.
    pub selector: SubscriptionSelector,
    /// Per-workflow target, when the subscription is tied to one workflow.
    pub workflow_target: Option<WorkflowId>,
    /// Recorded history slice replayed before the live tail. Empty unless the
    /// request carried a per-workflow resume cursor.
    pub replay: Vec<aion_core::Event>,
    /// Live event stream obtained from `Engine::subscribe` after authorization.
    /// When a resume cursor is present this tail is already deduplicated
    /// against `replay` (`seq > snapshot head`).
    pub events: BoxStream<'static, Result<aion_core::Event, aion::EventStreamLagged>>,
}

/// Authorize a subscription request and obtain the engine event tail.
///
/// ANTI-LEAK ORDERING: the namespace guard verdict comes first — before any
/// history read and before any resume-cursor validation. A caller without
/// grants probing a foreign or nonexistent workflow with any cursor receives
/// exactly the guard's `not_found`, never a cursor error that would disclose
/// existence or history length.
///
/// For resume requests the live broadcast subscription is attached *before*
/// the history snapshot is read (subscribe-then-snapshot), which is one half
/// of the gap-free splice proof in [`super::resume`].
///
/// # Errors
///
/// Returns [`ServerError`] when the request omits its variant, carries invalid
/// wire identifiers/selectors, is not authorized for the requested namespace,
/// the scoped engine handle is unavailable, the history snapshot cannot be
/// read, or the resume cursor is invalid for the recorded history.
pub async fn subscribe_events(
    guard: &NamespaceGuard,
    caller: &CallerIdentity,
    request: &SubscriptionRequest,
) -> Result<EventSubscription, ServerError> {
    let mapped = map_subscription_request(request)?;
    let target = mapped
        .workflow_target
        .as_ref()
        .map(WorkflowTarget::workflow);
    let scope = SubscriptionScope::from_request(request, target)?;
    let operation = NamespaceOperation::subscribe(scope, &mapped.filter);
    // Guard verdict FIRST: nothing below runs for an unauthorized caller.
    let scoped = guard.scope(caller, &operation).await?;
    let engine = scoped.engine()?;
    // T0: attach to the live broadcast before the snapshot is taken.
    let live = engine.subscribe(mapped.filter.clone());
    let (replay, events) = match (mapped.workflow_target.as_ref(), mapped.resume_from) {
        (Some(workflow_id), Some(resume_from_seq)) => {
            // T1 (> T0): snapshot the recorded history, then validate the
            // cursor against its head and build the dedupe splice.
            let history = engine.store().read_history(workflow_id).await?;
            super::resume::splice(live, history, resume_from_seq)?
        }
        _ => (Vec::new(), live),
    };

    Ok(EventSubscription {
        namespace: scoped.namespace().to_owned(),
        filter: mapped.filter,
        selector: mapped.selector,
        workflow_target: mapped.workflow_target,
        replay,
        events,
    })
}

/// Engine filter and metadata decoded from a subscription request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappedSubscription {
    /// Filter passed directly to `Engine::subscribe`.
    pub filter: EventFilter,
    /// Workflow-type/status selectors enforced server-side at the socket seam
    /// (the engine filter has no type or status dimension).
    pub selector: SubscriptionSelector,
    /// Per-workflow target, when supplied by the request.
    pub workflow_target: Option<WorkflowId>,
    /// Resume cursor ("first seq wanted"); carried by per-workflow
    /// subscriptions only — filtered/firehose streams are live-only.
    pub resume_from: Option<u64>,
}

/// Map a wire subscription request onto the engine's event filter surface.
///
/// The current engine filter supports workflow/run/family constraints only.
/// The namespace dimension is enforced by the guard plus the per-event
/// namespace gate; the workflow-type and status selectors are carried as a
/// [`SubscriptionSelector`] and enforced server-side at the socket seam before
/// any frame is encoded.
///
/// # Errors
///
/// Returns [`ServerError::Wire`] when the request omits its variant, omits the
/// required per-workflow id, or carries an invalid proto identifier/status.
pub fn map_subscription_request(
    request: &SubscriptionRequest,
) -> Result<MappedSubscription, ServerError> {
    match &request.subscription {
        Some(subscription_request::Subscription::PerWorkflow(subscription)) => {
            let workflow_id = decode_workflow_id(subscription.workflow_id.clone())?;
            Ok(MappedSubscription {
                filter: EventFilter {
                    workflow_id: Some(workflow_id.clone()),
                    ..EventFilter::default()
                },
                selector: SubscriptionSelector::unrestricted(),
                workflow_target: Some(workflow_id),
                resume_from: subscription.resume_from_seq,
            })
        }
        Some(subscription_request::Subscription::Filtered(subscription)) => {
            let status = decode_status(subscription.status)?;
            Ok(MappedSubscription {
                filter: EventFilter::default(),
                selector: SubscriptionSelector {
                    workflow_type: subscription.workflow_type.clone(),
                    status,
                },
                workflow_target: None,
                resume_from: None,
            })
        }
        Some(subscription_request::Subscription::Firehose(_subscription)) => {
            Ok(MappedSubscription {
                filter: EventFilter::default(),
                selector: SubscriptionSelector::unrestricted(),
                workflow_target: None,
                resume_from: None,
            })
        }
        None => Err(ServerError::Wire {
            wire: aion_proto::WireError::backend("subscription variant is missing"),
        }),
    }
}

fn decode_workflow_id(workflow_id: Option<ProtoWorkflowId>) -> Result<WorkflowId, ServerError> {
    workflow_id
        .ok_or_else(|| ServerError::Wire {
            wire: aion_proto::WireError::backend("per-workflow subscription id is missing"),
        })?
        .try_into()
        .map_err(|wire| ServerError::Wire { wire })
}

fn decode_status(status: Option<i32>) -> Result<Option<aion_core::WorkflowStatus>, ServerError> {
    let Some(status) = status else {
        return Ok(None);
    };
    let proto = ProtoWorkflowStatus::try_from(status).map_err(|_| ServerError::Wire {
        wire: aion_proto::WireError::backend("workflow status is invalid"),
    })?;
    let status =
        aion_core::WorkflowStatus::try_from(proto).map_err(|wire| ServerError::Wire { wire })?;
    Ok(Some(status))
}

#[cfg(test)]
mod tests {
    use aion_core::WorkflowId;
    use aion_proto::{
        FilteredSubscription, FirehoseSubscription, PerWorkflowSubscription, ProtoWorkflowId,
        SubscriptionRequest, subscription_request,
    };

    use super::map_subscription_request;
    use crate::config::NamespaceMode;
    use crate::namespace::{
        CallerIdentity, NamespaceGuard, NamespaceResolver, StaticScheduleNamespaces,
        StaticWorkflowNamespaces,
    };

    fn workflow_id() -> WorkflowId {
        WorkflowId::new_v4()
    }

    fn per_workflow_request(workflow_id: WorkflowId, namespace: &str) -> SubscriptionRequest {
        per_workflow_resume_request(workflow_id, namespace, None)
    }

    fn per_workflow_resume_request(
        workflow_id: WorkflowId,
        namespace: &str,
        resume_from_seq: Option<u64>,
    ) -> SubscriptionRequest {
        SubscriptionRequest {
            subscription: Some(subscription_request::Subscription::PerWorkflow(
                PerWorkflowSubscription {
                    namespace: namespace.to_owned(),
                    workflow_id: Some(ProtoWorkflowId::from(workflow_id)),
                    resume_from_seq,
                },
            )),
        }
    }

    fn filtered_request(namespace: &str, selector: Option<&str>) -> SubscriptionRequest {
        SubscriptionRequest {
            subscription: Some(subscription_request::Subscription::Filtered(
                FilteredSubscription {
                    namespace: namespace.to_owned(),
                    workflow_type: Some("checkout".to_owned()),
                    status: Some(aion_proto::ProtoWorkflowStatus::Running as i32),
                    namespace_selector: selector.map(str::to_owned),
                },
            )),
        }
    }

    fn firehose_request(namespace: &str) -> SubscriptionRequest {
        SubscriptionRequest {
            subscription: Some(subscription_request::Subscription::Firehose(
                FirehoseSubscription {
                    namespace: namespace.to_owned(),
                },
            )),
        }
    }

    fn guard() -> NamespaceGuard {
        NamespaceGuard::new(NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            StaticWorkflowNamespaces::default(),
            StaticScheduleNamespaces::default(),
        ))
    }

    fn caller() -> CallerIdentity {
        CallerIdentity::new("alice", ["tenant-a".to_owned()])
    }

    #[test]
    fn maps_per_workflow_subscription_to_workflow_filter() -> Result<(), Box<dyn std::error::Error>>
    {
        let workflow_id = workflow_id();
        let request = per_workflow_request(workflow_id.clone(), "tenant-a");

        let mapped = map_subscription_request(&request)?;

        assert_eq!(mapped.filter.workflow_id, Some(workflow_id.clone()));
        assert_eq!(mapped.workflow_target, Some(workflow_id));
        assert!(mapped.filter.run.is_none());
        assert!(mapped.filter.family.is_none());
        Ok(())
    }

    /// FINDING M2: filtered-subscription selectors must be carried into the
    /// mapped subscription, never validated-then-discarded — a discarded
    /// selector silently turns a filtered stream into a namespace firehose.
    #[test]
    fn maps_filtered_subscription_selectors_into_the_server_side_selector()
    -> Result<(), Box<dyn std::error::Error>> {
        let mapped = map_subscription_request(&filtered_request("tenant-a", Some("tenant-a")))?;

        assert_eq!(mapped.filter, aion::EventFilter::default());
        assert!(mapped.workflow_target.is_none());
        assert_eq!(
            mapped.selector,
            crate::stream::selector::SubscriptionSelector {
                workflow_type: Some("checkout".to_owned()),
                status: Some(aion_core::WorkflowStatus::Running),
            }
        );
        Ok(())
    }

    #[test]
    fn maps_firehose_subscription_to_engine_firehose_filter()
    -> Result<(), Box<dyn std::error::Error>> {
        let mapped = map_subscription_request(&firehose_request("tenant-a"))?;

        assert_eq!(mapped.filter, aion::EventFilter::default());
        assert!(mapped.workflow_target.is_none());
        assert_eq!(
            mapped.selector,
            crate::stream::selector::SubscriptionSelector::unrestricted()
        );
        Ok(())
    }

    #[tokio::test]
    async fn cross_namespace_subscription_selector_is_denied_before_engine_access()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = filtered_request("tenant-a", Some("tenant-b"));
        let mapped = map_subscription_request(&request)?;
        let scope = crate::namespace::SubscriptionScope::from_request(&request, None)?;
        let operation = crate::namespace::NamespaceOperation::subscribe(scope, &mapped.filter);

        let error = guard().scope(&caller(), &operation).await.err();

        assert!(matches!(error, Some(crate::ServerError::Namespace { .. })));
        Ok(())
    }

    #[tokio::test]
    async fn cross_namespace_firehose_is_denied_before_engine_access()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = firehose_request("tenant-b");
        let mapped = map_subscription_request(&request)?;
        let scope = crate::namespace::SubscriptionScope::from_request(&request, None)?;
        let operation = crate::namespace::NamespaceOperation::subscribe(scope, &mapped.filter);

        let error = guard().scope(&caller(), &operation).await.err();

        assert!(matches!(error, Some(crate::ServerError::Namespace { .. })));
        Ok(())
    }

    #[test]
    fn resume_cursor_is_carried_for_per_workflow_subscriptions_only()
    -> Result<(), Box<dyn std::error::Error>> {
        let with_cursor = map_subscription_request(&per_workflow_resume_request(
            workflow_id(),
            "tenant-a",
            Some(42),
        ))?;
        let without_cursor =
            map_subscription_request(&per_workflow_request(workflow_id(), "tenant-a"))?;
        let filtered = map_subscription_request(&filtered_request("tenant-a", None))?;
        let firehose = map_subscription_request(&firehose_request("tenant-a"))?;

        assert_eq!(with_cursor.resume_from, Some(42));
        assert_eq!(without_cursor.resume_from, None);
        assert_eq!(filtered.resume_from, None, "filtered streams are live-only");
        assert_eq!(firehose.resume_from, None, "firehose streams are live-only");
        Ok(())
    }

    /// ANTI-LEAK PIN: the guard verdict precedes every cursor inspection. A
    /// caller probing a foreign-owned workflow with any cursor — including
    /// cursors that would otherwise be `invalid_input` (0, absurdly large) —
    /// receives a `not_found` byte-identical to probing a workflow that never
    /// existed. An `invalid_input` here would disclose existence and history
    /// length across namespaces.
    #[tokio::test]
    async fn guard_verdict_precedes_cursor_validation_for_foreign_workflows()
    -> Result<(), Box<dyn std::error::Error>> {
        let foreign_workflow = workflow_id();
        let ownership = crate::namespace::StaticWorkflowNamespaces::default();
        ownership.record(foreign_workflow.clone(), "tenant-b")?;
        let guard = NamespaceGuard::new(NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            ownership,
            StaticScheduleNamespaces::default(),
        ));
        let caller = caller();

        let mut wire_errors = Vec::new();
        for cursor in [Some(0), Some(1), Some(u64::MAX), None] {
            let request = per_workflow_resume_request(foreign_workflow.clone(), "tenant-a", cursor);
            let error = super::subscribe_events(&guard, &caller, &request)
                .await
                .err()
                .map(|error| error.to_wire_error())
                .ok_or_else(|| format!("foreign probe with cursor {cursor:?} must be rejected"))?;
            wire_errors.push(error);
        }
        // A probe of a workflow that never existed, with an absurd cursor.
        let absent = super::subscribe_events(
            &guard,
            &caller,
            &per_workflow_resume_request(WorkflowId::new_v4(), "tenant-a", Some(u64::MAX)),
        )
        .await
        .err()
        .map(|error| error.to_wire_error())
        .ok_or("nonexistent-workflow probe must be rejected")?;

        assert_eq!(absent.code, aion_proto::WireErrorCode::NotFound);
        for error in &wire_errors {
            assert_eq!(
                error, &absent,
                "every foreign probe must be byte-identical to the nonexistent probe"
            );
        }
        Ok(())
    }
}
