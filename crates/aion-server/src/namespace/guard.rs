//! Adapter-boundary namespace enforcement.

use aion::EventFilter;
use aion_core::{RunId, WorkflowFilter, WorkflowId};
use aion_proto::{
    FilteredSubscription, FirehoseSubscription, PerWorkflowSubscription, ProtoCancelRequest,
    ProtoCountWorkflowsRequest, ProtoDescribeWorkflowRequest, ProtoListWorkflowsRequest,
    ProtoQueryRequest, ProtoRegisterWorker, ProtoSignalRequest, ProtoStartWorkflowRequest,
    SubscriptionRequest, subscription_request,
};

use crate::error::ServerError;

use super::resolver::{CallerIdentity, NamespaceResolver, ScopedEngine};

/// Adapter-boundary guard shared by API, stream, and worker transports.
#[derive(Clone)]
pub struct NamespaceGuard {
    resolver: NamespaceResolver,
}

impl NamespaceGuard {
    /// Build a guard from the shared namespace resolver.
    #[must_use]
    pub const fn new(resolver: NamespaceResolver) -> Self {
        Self { resolver }
    }

    /// Borrow the resolver backing this guard.
    #[must_use]
    pub const fn resolver(&self) -> &NamespaceResolver {
        &self.resolver
    }

    /// Authorize and scope an operation before any engine method can be called.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Namespace`] when the caller cannot access the
    /// requested namespace, when a subscription selects another namespace, or
    /// when a targeted workflow is not owned by the authorized namespace.
    pub fn scope(
        &self,
        caller: &CallerIdentity,
        operation: &NamespaceOperation<'_>,
    ) -> Result<ScopedEngine, ServerError> {
        let requested_namespace = operation.requested_namespace();
        let scoped = self.resolver.resolve(caller, requested_namespace)?;
        operation.verify(&self.resolver, scoped.namespace())?;
        Ok(scoped)
    }
}

/// Namespace-sensitive operation described at the adapter boundary.
pub enum NamespaceOperation<'a> {
    /// Start workflow request.
    StartWorkflow(&'a ProtoStartWorkflowRequest),
    /// Signal workflow request.
    Signal(&'a ProtoSignalRequest, WorkflowTarget<'a>),
    /// Query workflow request.
    Query(&'a ProtoQueryRequest, WorkflowTarget<'a>),
    /// Cancel workflow request.
    Cancel(&'a ProtoCancelRequest, WorkflowTarget<'a>),
    /// List workflow request.
    ListWorkflows(&'a ProtoListWorkflowsRequest, &'a WorkflowFilter),
    /// Count workflow request.
    CountWorkflows(&'a ProtoCountWorkflowsRequest),
    /// Describe workflow request.
    Describe(&'a ProtoDescribeWorkflowRequest, WorkflowTarget<'a>),
    /// Event subscription request.
    Subscribe(SubscriptionScope<'a>, &'a EventFilter),
    /// Worker registration request.
    RegisterWorker(&'a ProtoRegisterWorker),
}

impl<'a> NamespaceOperation<'a> {
    /// Create a start-workflow operation descriptor.
    #[must_use]
    pub const fn start(request: &'a ProtoStartWorkflowRequest) -> Self {
        Self::StartWorkflow(request)
    }

    /// Create a signal operation descriptor.
    #[must_use]
    pub const fn signal(request: &'a ProtoSignalRequest, target: WorkflowTarget<'a>) -> Self {
        Self::Signal(request, target)
    }

    /// Create a query operation descriptor.
    #[must_use]
    pub const fn query(request: &'a ProtoQueryRequest, target: WorkflowTarget<'a>) -> Self {
        Self::Query(request, target)
    }

    /// Create a cancel operation descriptor.
    #[must_use]
    pub const fn cancel(request: &'a ProtoCancelRequest, target: WorkflowTarget<'a>) -> Self {
        Self::Cancel(request, target)
    }

    /// Create a list-workflows operation descriptor.
    #[must_use]
    pub const fn list(request: &'a ProtoListWorkflowsRequest, filter: &'a WorkflowFilter) -> Self {
        Self::ListWorkflows(request, filter)
    }

    /// Create a count-workflows operation descriptor.
    #[must_use]
    pub const fn count(request: &'a ProtoCountWorkflowsRequest) -> Self {
        Self::CountWorkflows(request)
    }

    /// Create a describe-workflow operation descriptor.
    #[must_use]
    pub const fn describe(
        request: &'a ProtoDescribeWorkflowRequest,
        target: WorkflowTarget<'a>,
    ) -> Self {
        Self::Describe(request, target)
    }

    /// Create a subscribe operation descriptor.
    #[must_use]
    pub const fn subscribe(scope: SubscriptionScope<'a>, filter: &'a EventFilter) -> Self {
        Self::Subscribe(scope, filter)
    }

    /// Create a worker-registration operation descriptor.
    #[must_use]
    pub const fn register_worker(request: &'a ProtoRegisterWorker) -> Self {
        Self::RegisterWorker(request)
    }

    fn requested_namespace(&self) -> &str {
        match self {
            Self::StartWorkflow(request) => request.namespace.as_str(),
            Self::Signal(request, _target) => request.namespace.as_str(),
            Self::Query(request, _target) => request.namespace.as_str(),
            Self::Cancel(request, _target) => request.namespace.as_str(),
            Self::ListWorkflows(request, _filter) => request.namespace.as_str(),
            Self::CountWorkflows(request) => request.namespace.as_str(),
            Self::Describe(request, _target) => request.namespace.as_str(),
            Self::Subscribe(scope, _filter) => scope.namespace(),
            Self::RegisterWorker(request) => request.namespace.as_str(),
        }
    }

    fn verify(
        &self,
        resolver: &NamespaceResolver,
        authorized_namespace: &str,
    ) -> Result<(), ServerError> {
        match self {
            Self::Signal(_, target)
            | Self::Query(_, target)
            | Self::Cancel(_, target)
            | Self::Describe(_, target) => target.verify(resolver, authorized_namespace),
            Self::Subscribe(scope, filter) => scope.verify(resolver, authorized_namespace, filter),
            Self::StartWorkflow(_)
            | Self::ListWorkflows(_, _)
            | Self::CountWorkflows(_)
            | Self::RegisterWorker(_) => Ok(()),
        }
    }
}

/// Target workflow identifiers decoded by a handler before the engine call.
#[derive(Clone, Copy)]
pub struct WorkflowTarget<'a> {
    workflow_id: &'a WorkflowId,
    run_id: Option<&'a RunId>,
}

impl<'a> WorkflowTarget<'a> {
    /// Build a target for operations that require workflow and run identifiers.
    #[must_use]
    pub const fn with_run(workflow_id: &'a WorkflowId, run_id: &'a RunId) -> Self {
        Self {
            workflow_id,
            run_id: Some(run_id),
        }
    }

    /// Build a target for operations that identify only a workflow.
    #[must_use]
    pub const fn workflow(workflow_id: &'a WorkflowId) -> Self {
        Self {
            workflow_id,
            run_id: None,
        }
    }

    /// Target workflow id.
    #[must_use]
    pub const fn workflow_id(&self) -> &WorkflowId {
        self.workflow_id
    }

    /// Optional target run id.
    #[must_use]
    pub const fn run_id(&self) -> Option<&RunId> {
        self.run_id
    }

    fn verify(&self, resolver: &NamespaceResolver, namespace: &str) -> Result<(), ServerError> {
        resolver.verify_workflow_ownership(namespace, self.workflow_id)
    }
}

/// Event subscription namespace scope decoded by a stream adapter.
pub enum SubscriptionScope<'a> {
    /// Events for one workflow.
    PerWorkflow(&'a PerWorkflowSubscription, WorkflowTarget<'a>),
    /// Filtered events in the caller namespace.
    Filtered(&'a FilteredSubscription),
    /// Firehose events in the caller namespace.
    Firehose(&'a FirehoseSubscription),
}

impl<'a> SubscriptionScope<'a> {
    /// Decode the namespace scope from a subscription request after the handler
    /// has decoded any workflow identifiers required by the selected variant.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Namespace`] when the request omits the subscription
    /// variant.
    pub fn from_request(
        request: &'a SubscriptionRequest,
        workflow_target: Option<WorkflowTarget<'a>>,
    ) -> Result<Self, ServerError> {
        match &request.subscription {
            Some(subscription_request::Subscription::PerWorkflow(subscription)) => {
                let target = workflow_target.ok_or_else(|| {
                    ServerError::namespace_denied(
                        "per-workflow subscription target must be decoded before guard scope",
                    )
                })?;
                Ok(Self::PerWorkflow(subscription, target))
            }
            Some(subscription_request::Subscription::Filtered(subscription)) => {
                Ok(Self::Filtered(subscription))
            }
            Some(subscription_request::Subscription::Firehose(subscription)) => {
                Ok(Self::Firehose(subscription))
            }
            None => Err(ServerError::namespace_denied(
                "subscription request must name a namespace",
            )),
        }
    }

    fn namespace(&self) -> &str {
        match self {
            Self::PerWorkflow(subscription, _target) => subscription.namespace.as_str(),
            Self::Filtered(subscription) => subscription.namespace.as_str(),
            Self::Firehose(subscription) => subscription.namespace.as_str(),
        }
    }

    fn verify(
        &self,
        resolver: &NamespaceResolver,
        namespace: &str,
        filter: &EventFilter,
    ) -> Result<(), ServerError> {
        match self {
            Self::PerWorkflow(_subscription, target) => {
                verify_subscription_filter_target(filter, Some(*target), resolver, namespace)
            }
            Self::Filtered(subscription) => {
                verify_namespace_selector(subscription.namespace_selector.as_deref(), namespace)?;
                verify_subscription_filter_target(filter, None, resolver, namespace)
            }
            Self::Firehose(_) => {
                verify_subscription_filter_target(filter, None, resolver, namespace)
            }
        }
    }
}

fn verify_namespace_selector(selector: Option<&str>, namespace: &str) -> Result<(), ServerError> {
    match selector {
        Some(selector) if selector != namespace => Err(ServerError::namespace_denied(
            "subscription namespace selector is not authorized",
        )),
        Some(_) | None => Ok(()),
    }
}

fn verify_subscription_filter_target(
    filter: &EventFilter,
    explicit_target: Option<WorkflowTarget<'_>>,
    resolver: &NamespaceResolver,
    namespace: &str,
) -> Result<(), ServerError> {
    if let Some(target) = explicit_target {
        if filter
            .workflow_id
            .as_ref()
            .is_some_and(|workflow_id| workflow_id != target.workflow_id())
        {
            return Err(ServerError::namespace_denied(
                "subscription filter workflow does not match decoded target",
            ));
        }
        target.verify(resolver, namespace)
    } else if let Some(workflow_id) = &filter.workflow_id {
        resolver.verify_workflow_ownership(namespace, workflow_id)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use aion_core::{RunId, WorkflowFilter, WorkflowId};
    use aion_proto::{
        FilteredSubscription, FirehoseSubscription, PerWorkflowSubscription, ProtoCancelRequest,
        ProtoDescribeWorkflowRequest, ProtoListWorkflowsRequest, ProtoQueryRequest,
        ProtoRegisterWorker, ProtoSignalRequest, ProtoStartWorkflowRequest,
    };

    use super::{NamespaceGuard, NamespaceOperation, SubscriptionScope, WorkflowTarget};
    use crate::config::NamespaceMode;
    use crate::namespace::{CallerIdentity, NamespaceResolver, WorkflowOwnership};

    struct RecordingFakeEngine {
        calls: Mutex<Vec<&'static str>>,
    }

    impl RecordingFakeEngine {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Result<Vec<&'static str>, Box<dyn std::error::Error>> {
            let calls = self
                .calls
                .lock()
                .map_err(|_| "fake engine calls lock poisoned")?;
            Ok(calls.clone())
        }
    }

    fn guard_with_ownership(ownership: WorkflowOwnership) -> NamespaceGuard {
        let resolver =
            NamespaceResolver::authorization_only(NamespaceMode::SharedEngine, ownership);
        NamespaceGuard::new(resolver)
    }

    fn caller() -> CallerIdentity {
        CallerIdentity::new("alice", [String::from("tenant-a")])
    }

    fn workflow_ids() -> (WorkflowId, RunId) {
        (
            WorkflowId::new(uuid::Uuid::from_u128(1)),
            RunId::new(uuid::Uuid::from_u128(2)),
        )
    }

    #[test]
    fn denied_targeted_operations_do_not_call_engine() -> Result<(), Box<dyn std::error::Error>> {
        let (workflow_id, run_id) = workflow_ids();
        let ownership = WorkflowOwnership::default();
        ownership.record(workflow_id.clone(), "tenant-b")?;
        let guard = guard_with_ownership(ownership);
        let fake = RecordingFakeEngine::new();
        let target = WorkflowTarget::with_run(&workflow_id, &run_id);

        let signal = ProtoSignalRequest {
            namespace: String::from("tenant-a"),
            workflow_id: None,
            run_id: None,
            signal_name: String::from("ship"),
            payload: None,
        };
        let query = ProtoQueryRequest {
            namespace: String::from("tenant-a"),
            workflow_id: None,
            run_id: None,
            query_name: String::from("state"),
        };
        let cancel = ProtoCancelRequest {
            namespace: String::from("tenant-a"),
            workflow_id: None,
            run_id: None,
            reason: String::from("operator"),
        };
        let describe = ProtoDescribeWorkflowRequest {
            namespace: String::from("tenant-a"),
            workflow_id: None,
            run_id: None,
            include_history: false,
        };

        let operations = [
            NamespaceOperation::signal(&signal, target),
            NamespaceOperation::query(&query, target),
            NamespaceOperation::cancel(&cancel, target),
            NamespaceOperation::describe(&describe, target),
        ];

        for operation in operations {
            let result = guard.scope(&caller(), &operation);
            assert!(result.is_err());
        }
        assert!(fake.calls()?.is_empty());
        Ok(())
    }

    #[test]
    fn denied_list_subscribe_and_worker_scope_do_not_call_engine()
    -> Result<(), Box<dyn std::error::Error>> {
        let (workflow_id, run_id) = workflow_ids();
        let ownership = WorkflowOwnership::default();
        ownership.record(workflow_id.clone(), "tenant-b")?;
        let guard = guard_with_ownership(ownership);
        let fake = RecordingFakeEngine::new();
        let filter = WorkflowFilter::default();
        let event_filter = aion::EventFilter::default();

        let list = ProtoListWorkflowsRequest {
            namespace: String::from("tenant-b"),
            filter: None,
        };
        let filtered = FilteredSubscription {
            namespace: String::from("tenant-a"),
            workflow_type: None,
            status: None,
            namespace_selector: Some(String::from("tenant-b")),
        };
        let filtered_by_workflow = FilteredSubscription {
            namespace: String::from("tenant-a"),
            workflow_type: None,
            status: None,
            namespace_selector: None,
        };
        let per_workflow = PerWorkflowSubscription {
            namespace: String::from("tenant-a"),
            workflow_id: None,
        };
        let cross_namespace_filter = aion::EventFilter {
            workflow_id: Some(workflow_id.clone()),
            run: None,
            family: None,
        };
        let firehose = FirehoseSubscription {
            namespace: String::from("tenant-b"),
        };
        let worker = ProtoRegisterWorker {
            namespace: String::from("tenant-b"),
            activity_types: vec![String::from("ship")],
        };

        let target = WorkflowTarget::with_run(&workflow_id, &run_id);

        assert!(
            guard
                .scope(&caller(), &NamespaceOperation::list(&list, &filter))
                .is_err()
        );
        assert!(
            guard
                .scope(
                    &caller(),
                    &NamespaceOperation::subscribe(
                        SubscriptionScope::Filtered(&filtered),
                        &event_filter,
                    ),
                )
                .is_err()
        );
        assert!(
            guard
                .scope(
                    &caller(),
                    &NamespaceOperation::subscribe(
                        SubscriptionScope::Filtered(&filtered_by_workflow),
                        &cross_namespace_filter,
                    ),
                )
                .is_err()
        );
        assert!(
            guard
                .scope(
                    &caller(),
                    &NamespaceOperation::subscribe(
                        SubscriptionScope::PerWorkflow(&per_workflow, target),
                        &cross_namespace_filter,
                    ),
                )
                .is_err()
        );
        assert!(
            guard
                .scope(
                    &caller(),
                    &NamespaceOperation::subscribe(
                        SubscriptionScope::Firehose(&firehose),
                        &event_filter
                    ),
                )
                .is_err()
        );
        assert!(
            guard
                .scope(&caller(), &NamespaceOperation::register_worker(&worker),)
                .is_err()
        );
        assert!(fake.calls()?.is_empty());
        Ok(())
    }

    #[test]
    fn authorized_start_returns_scoped_engine() -> Result<(), Box<dyn std::error::Error>> {
        let guard = guard_with_ownership(WorkflowOwnership::default());
        let request = ProtoStartWorkflowRequest {
            namespace: String::from("tenant-a"),
            workflow_type: String::from("checkout"),
            input: None,
        };

        let scoped = guard.scope(&caller(), &NamespaceOperation::start(&request))?;

        assert_eq!(scoped.namespace(), "tenant-a");
        Ok(())
    }

    #[test]
    fn single_tenant_mode_authorizes_configured_namespace() -> Result<(), Box<dyn std::error::Error>>
    {
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SingleTenant {
                namespace: String::from("tenant-a"),
            },
            WorkflowOwnership::default(),
        );
        let guard = NamespaceGuard::new(resolver);
        let request = ProtoRegisterWorker {
            namespace: String::from("tenant-a"),
            activity_types: Vec::new(),
        };

        let scoped = guard.scope(
            &CallerIdentity::new("single-tenant", Vec::<String>::new()),
            &NamespaceOperation::register_worker(&request),
        )?;

        assert_eq!(scoped.namespace(), "tenant-a");
        Ok(())
    }
}
