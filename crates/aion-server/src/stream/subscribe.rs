//! `SubscriptionRequest` to `EventFilter` mapping.

use aion::EventFilter;
use aion_core::WorkflowId;
use aion_proto::{ProtoWorkflowId, ProtoWorkflowStatus, SubscriptionRequest, subscription_request};
use futures::stream::BoxStream;

use crate::error::ServerError;
use crate::namespace::{
    CallerIdentity, NamespaceGuard, NamespaceOperation, SubscriptionScope, WorkflowTarget,
};

/// Authorized subscription returned by the adapter boundary.
pub struct EventSubscription {
    /// Namespace authorized by the guard.
    pub namespace: String,
    /// Engine-side filter used for the subscription.
    pub filter: EventFilter,
    /// Per-workflow target, when the subscription is tied to one workflow.
    pub workflow_target: Option<WorkflowId>,
    /// Live event stream obtained from `Engine::subscribe` after authorization.
    pub events: BoxStream<'static, aion_core::Event>,
}

/// Authorize a subscription request and obtain the engine event tail.
///
/// # Errors
///
/// Returns [`ServerError`] when the request omits its variant, carries invalid
/// wire identifiers/selectors, is not authorized for the requested namespace, or
/// the scoped engine handle is unavailable.
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
    let scoped = guard.scope(caller, &operation).await?;
    let events = scoped.engine()?.subscribe(mapped.filter.clone());

    Ok(EventSubscription {
        namespace: scoped.namespace().to_owned(),
        filter: mapped.filter,
        workflow_target: mapped.workflow_target,
        events,
    })
}

/// Engine filter and metadata decoded from a subscription request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappedSubscription {
    /// Filter passed directly to `Engine::subscribe`.
    pub filter: EventFilter,
    /// Per-workflow target, when supplied by the request.
    pub workflow_target: Option<WorkflowId>,
}

/// Map a wire subscription request onto the engine's event filter surface.
///
/// The current engine filter supports workflow/run/family constraints only. The
/// namespace, workflow-type, and status selectors remain guard-scoped adapter
/// metadata; they are validated before subscribe but are not reimplemented as a
/// server-side broadcast filter.
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
                workflow_target: Some(workflow_id),
            })
        }
        Some(subscription_request::Subscription::Filtered(subscription)) => {
            decode_status(subscription.status)?;
            Ok(MappedSubscription {
                filter: EventFilter::default(),
                workflow_target: None,
            })
        }
        Some(subscription_request::Subscription::Firehose(_subscription)) => {
            Ok(MappedSubscription {
                filter: EventFilter::default(),
                workflow_target: None,
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

fn decode_status(status: Option<i32>) -> Result<(), ServerError> {
    let Some(status) = status else {
        return Ok(());
    };
    let proto = ProtoWorkflowStatus::try_from(status).map_err(|_| ServerError::Wire {
        wire: aion_proto::WireError::backend("workflow status is invalid"),
    })?;
    let status =
        aion_core::WorkflowStatus::try_from(proto).map_err(|wire| ServerError::Wire { wire })?;
    let _ = status;
    Ok(())
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
        SubscriptionRequest {
            subscription: Some(subscription_request::Subscription::PerWorkflow(
                PerWorkflowSubscription {
                    namespace: namespace.to_owned(),
                    workflow_id: Some(ProtoWorkflowId::from(workflow_id)),
                    resume_from_seq: None,
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

    #[test]
    fn maps_filtered_subscription_to_engine_firehose_filter()
    -> Result<(), Box<dyn std::error::Error>> {
        let mapped = map_subscription_request(&filtered_request("tenant-a", Some("tenant-a")))?;

        assert_eq!(mapped.filter, aion::EventFilter::default());
        assert!(mapped.workflow_target.is_none());
        Ok(())
    }

    #[test]
    fn maps_firehose_subscription_to_engine_firehose_filter()
    -> Result<(), Box<dyn std::error::Error>> {
        let mapped = map_subscription_request(&firehose_request("tenant-a"))?;

        assert_eq!(mapped.filter, aion::EventFilter::default());
        assert!(mapped.workflow_target.is_none());
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
}
