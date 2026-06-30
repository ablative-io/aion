//! Adapter-boundary namespace enforcement.

use aion::EventFilter;
use aion_core::{RunId, ScheduleId, WorkflowFilter, WorkflowId};
use aion_proto::{
    FilteredSubscription, FirehoseSubscription, PerWorkflowSubscription, ProtoCancelRequest,
    ProtoCountWorkflowsRequest, ProtoCreateScheduleRequest, ProtoDescribeWorkflowRequest,
    ProtoListSchedulesRequest, ProtoListWorkflowsRequest, ProtoQueryRequest, ProtoRegisterWorker,
    ProtoScheduleIdRequest, ProtoSignalRequest, ProtoStartWorkflowRequest,
    ProtoUpdateScheduleRequest, SubscriptionRequest, subscription_request,
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
    /// Workflow-targeted operations verify durable ownership, which reads the
    /// target workflow's recorded history through the resolver's ownership
    /// source.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Namespace`] (`namespace_denied`) when the caller
    /// has no grant for the requested namespace or a subscription selects
    /// another namespace. Returns a `not_found` wire error when the requested
    /// namespace is granted but a targeted workflow is not visible in it —
    /// foreign-owned and nonexistent workflows are deliberately
    /// indistinguishable so the guard never leaks cross-tenant existence.
    pub async fn scope(
        &self,
        caller: &CallerIdentity,
        operation: &NamespaceOperation<'_>,
    ) -> Result<ScopedEngine, ServerError> {
        let requested_namespace = operation.requested_namespace();
        let scoped = self.resolver.resolve(caller, requested_namespace)?;
        operation.verify(&self.resolver, scoped.namespace()).await?;
        Ok(scoped)
    }

    /// Authorize a caller for a single namespace by name, returning the
    /// resolved namespace if the grant allows it.
    ///
    /// This is the SAME grant check the access hop runs ([`NamespaceResolver::resolve`]):
    /// the operator (all-namespaces) is authorized for any name, an enumerated
    /// caller only for a granted one, and single-tenant mode only for the
    /// configured namespace. It carries no workflow/schedule target, so it never
    /// reaches durable ownership — the control-plane create path (`POST
    /// /namespaces`) authorizes namespace *existence*, not a per-resource probe.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Namespace`] (`namespace_denied`) when the caller
    /// has no grant for `namespace`, so an unauthorized caller can never create
    /// (or learn the existence of) a namespace it cannot access.
    pub fn authorize_namespace(
        &self,
        caller: &CallerIdentity,
        namespace: &str,
    ) -> Result<String, ServerError> {
        let scoped = self.resolver.resolve(caller, namespace)?;
        Ok(scoped.namespace().to_owned())
    }

    /// Authorize every namespace in a worker registration's set, returning the
    /// resolved namespaces in stable wire order with duplicates removed.
    ///
    /// A worker serves a SET of namespaces (NODE affinity model). Each one is an
    /// independent correctness boundary, so the worker is authorized for it
    /// exactly as a single-namespace operation would be: the whole registration
    /// is denied if the caller lacks a grant for any namespace in the set. The
    /// set must be non-empty.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Namespace`] (`namespace_denied`) when the set is
    /// empty or the caller has no grant for some namespace in it.
    pub fn scope_worker_namespaces(
        &self,
        caller: &CallerIdentity,
        namespaces: &[String],
    ) -> Result<Vec<String>, ServerError> {
        if namespaces.is_empty() {
            return Err(ServerError::namespace_denied(
                "worker registration must name at least one namespace",
            ));
        }
        let mut authorized: Vec<String> = Vec::with_capacity(namespaces.len());
        for namespace in namespaces {
            let scoped = self.resolver.resolve(caller, namespace)?;
            let resolved = scoped.namespace().to_owned();
            if !authorized.contains(&resolved) {
                authorized.push(resolved);
            }
        }
        Ok(authorized)
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
    /// Create schedule request.
    CreateSchedule(&'a ProtoCreateScheduleRequest),
    /// Update schedule request.
    UpdateSchedule(&'a ProtoUpdateScheduleRequest, ScheduleTarget<'a>),
    /// Pause schedule request.
    PauseSchedule(&'a ProtoScheduleIdRequest, ScheduleTarget<'a>),
    /// Resume schedule request.
    ResumeSchedule(&'a ProtoScheduleIdRequest, ScheduleTarget<'a>),
    /// Delete schedule request.
    DeleteSchedule(&'a ProtoScheduleIdRequest, ScheduleTarget<'a>),
    /// List schedules request.
    ListSchedules(&'a ProtoListSchedulesRequest),
    /// Describe schedule request.
    DescribeSchedule(&'a ProtoScheduleIdRequest, ScheduleTarget<'a>),
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

    /// Create a create-schedule operation descriptor.
    #[must_use]
    pub const fn create_schedule(request: &'a ProtoCreateScheduleRequest) -> Self {
        Self::CreateSchedule(request)
    }

    /// Create an update-schedule operation descriptor.
    #[must_use]
    pub const fn update_schedule(
        request: &'a ProtoUpdateScheduleRequest,
        target: ScheduleTarget<'a>,
    ) -> Self {
        Self::UpdateSchedule(request, target)
    }

    /// Create a pause-schedule operation descriptor.
    #[must_use]
    pub const fn pause_schedule(
        request: &'a ProtoScheduleIdRequest,
        target: ScheduleTarget<'a>,
    ) -> Self {
        Self::PauseSchedule(request, target)
    }

    /// Create a resume-schedule operation descriptor.
    #[must_use]
    pub const fn resume_schedule(
        request: &'a ProtoScheduleIdRequest,
        target: ScheduleTarget<'a>,
    ) -> Self {
        Self::ResumeSchedule(request, target)
    }

    /// Create a delete-schedule operation descriptor.
    #[must_use]
    pub const fn delete_schedule(
        request: &'a ProtoScheduleIdRequest,
        target: ScheduleTarget<'a>,
    ) -> Self {
        Self::DeleteSchedule(request, target)
    }

    /// Create a list-schedules operation descriptor.
    #[must_use]
    pub const fn list_schedules(request: &'a ProtoListSchedulesRequest) -> Self {
        Self::ListSchedules(request)
    }

    /// Create a describe-schedule operation descriptor.
    #[must_use]
    pub const fn describe_schedule(
        request: &'a ProtoScheduleIdRequest,
        target: ScheduleTarget<'a>,
    ) -> Self {
        Self::DescribeSchedule(request, target)
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
            Self::CreateSchedule(request) => request.namespace.as_str(),
            Self::UpdateSchedule(request, _target) => request.namespace.as_str(),
            Self::PauseSchedule(request, _target)
            | Self::ResumeSchedule(request, _target)
            | Self::DeleteSchedule(request, _target)
            | Self::DescribeSchedule(request, _target) => request.namespace.as_str(),
            Self::ListSchedules(request) => request.namespace.as_str(),
            Self::Subscribe(scope, _filter) => scope.namespace(),
            // A worker advertises a SET of namespaces; the first stands in for
            // the single-scope path. Multi-namespace worker registration
            // authorizes every namespace via `scope_worker_namespaces`, so this
            // is only reached if a caller routes a worker registration through
            // the single-scope `scope` API. Empty set => empty string, which
            // the resolver rejects as an unauthorized namespace.
            Self::RegisterWorker(request) => request.namespaces.first().map_or("", String::as_str),
        }
    }

    async fn verify(
        &self,
        resolver: &NamespaceResolver,
        authorized_namespace: &str,
    ) -> Result<(), ServerError> {
        match self {
            Self::Signal(_, target)
            | Self::Query(_, target)
            | Self::Cancel(_, target)
            | Self::Describe(_, target) => target.verify(resolver, authorized_namespace).await,
            Self::UpdateSchedule(_, target)
            | Self::PauseSchedule(_, target)
            | Self::ResumeSchedule(_, target)
            | Self::DeleteSchedule(_, target)
            | Self::DescribeSchedule(_, target) => {
                target.verify(resolver, authorized_namespace).await
            }
            Self::Subscribe(scope, filter) => {
                scope.verify(resolver, authorized_namespace, filter).await
            }
            // CreateSchedule needs no target verification: the schedule id is
            // server-generated at creation, so a create can never collide with
            // or probe another tenant's resource; the handler stamps the
            // authorized namespace into the recorded config. ListSchedules is
            // grant-checked here and result-filtered in the handler, exactly
            // like workflow list.
            Self::StartWorkflow(_)
            | Self::ListWorkflows(_, _)
            | Self::CountWorkflows(_)
            | Self::CreateSchedule(_)
            | Self::ListSchedules(_)
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

    async fn verify(
        &self,
        resolver: &NamespaceResolver,
        namespace: &str,
    ) -> Result<(), ServerError> {
        resolver
            .verify_workflow_ownership(namespace, self.workflow_id)
            .await
    }
}

/// Target schedule identifier decoded by a handler before the engine call.
#[derive(Clone, Copy)]
pub struct ScheduleTarget<'a> {
    schedule_id: &'a ScheduleId,
}

impl<'a> ScheduleTarget<'a> {
    /// Build a target for operations that identify one schedule.
    #[must_use]
    pub const fn schedule(schedule_id: &'a ScheduleId) -> Self {
        Self { schedule_id }
    }

    /// Target schedule id.
    #[must_use]
    pub const fn schedule_id(&self) -> &ScheduleId {
        self.schedule_id
    }

    async fn verify(
        &self,
        resolver: &NamespaceResolver,
        namespace: &str,
    ) -> Result<(), ServerError> {
        resolver
            .verify_schedule_ownership(namespace, self.schedule_id)
            .await
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
            // The cluster subscription is deployment-scoped, not namespace-scoped:
            // it is authorized by the deploy grant in the cluster channel, never
            // through this namespace `SubscriptionScope`. It is dispatched before
            // the workflow guard path runs, so reaching here is a routing bug.
            Some(subscription_request::Subscription::Cluster(_)) => {
                Err(ServerError::namespace_denied(
                    "cluster subscription is deployment-scoped and not served by the namespace guard",
                ))
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

    async fn verify(
        &self,
        resolver: &NamespaceResolver,
        namespace: &str,
        filter: &EventFilter,
    ) -> Result<(), ServerError> {
        match self {
            Self::PerWorkflow(_subscription, target) => {
                verify_subscription_filter_target(filter, Some(*target), resolver, namespace).await
            }
            Self::Filtered(subscription) => {
                verify_namespace_selector(subscription.namespace_selector.as_deref(), namespace)?;
                verify_subscription_filter_target(filter, None, resolver, namespace).await
            }
            Self::Firehose(_) => {
                verify_subscription_filter_target(filter, None, resolver, namespace).await
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

async fn verify_subscription_filter_target(
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
        target.verify(resolver, namespace).await
    } else if let Some(workflow_id) = &filter.workflow_id {
        resolver
            .verify_workflow_ownership(namespace, workflow_id)
            .await
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use aion_core::{RunId, ScheduleId, WorkflowFilter, WorkflowId};
    use aion_proto::{
        FilteredSubscription, FirehoseSubscription, PerWorkflowSubscription, ProtoCancelRequest,
        ProtoCreateScheduleRequest, ProtoDescribeWorkflowRequest, ProtoListSchedulesRequest,
        ProtoListWorkflowsRequest, ProtoQueryRequest, ProtoRegisterWorker, ProtoScheduleIdRequest,
        ProtoSignalRequest, ProtoStartWorkflowRequest, ProtoUpdateScheduleRequest,
    };
    use async_trait::async_trait;

    use super::{
        NamespaceGuard, NamespaceOperation, ScheduleTarget, SubscriptionScope, WorkflowTarget,
    };
    use crate::config::NamespaceMode;
    use crate::error::ServerError;
    use crate::namespace::{
        CallerIdentity, NamespaceResolver, ScheduleNamespaceSource, StaticScheduleNamespaces,
        StaticWorkflowNamespaces,
    };

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

    /// Schedule ownership source that counts every verification consult so
    /// tests can prove whether the guard reached durable ownership at all:
    /// ownership misses must consult exactly once per operation, while grant
    /// denials must short-circuit before any consult.
    #[derive(Clone)]
    struct CountingScheduleNamespaces {
        inner: StaticScheduleNamespaces,
        calls: Arc<AtomicUsize>,
    }

    impl CountingScheduleNamespaces {
        fn wrapping(inner: StaticScheduleNamespaces) -> Self {
            Self {
                inner,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ScheduleNamespaceSource for CountingScheduleNamespaces {
        async fn schedule_namespace(
            &self,
            schedule_id: &ScheduleId,
        ) -> Result<Option<String>, ServerError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.schedule_namespace(schedule_id).await
        }
    }

    fn guard_with_ownership(ownership: StaticWorkflowNamespaces) -> NamespaceGuard {
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            ownership,
            StaticScheduleNamespaces::default(),
        );
        NamespaceGuard::new(resolver)
    }

    fn guard_with_schedule_ownership(
        schedule_ownership: impl ScheduleNamespaceSource + 'static,
    ) -> NamespaceGuard {
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            StaticWorkflowNamespaces::default(),
            schedule_ownership,
        );
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

    #[tokio::test]
    async fn denied_targeted_operations_do_not_call_engine()
    -> Result<(), Box<dyn std::error::Error>> {
        let (workflow_id, run_id) = workflow_ids();
        let ownership = StaticWorkflowNamespaces::default();
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
            let result = guard.scope(&caller(), &operation).await;
            // Ownership miss in a granted namespace is NotFound, not
            // NamespaceDenied: cross-tenant probes must be indistinguishable
            // from nonexistent workflows.
            assert_eq!(
                result.err().map(|error| error.to_wire_error().code),
                Some(aion_proto::WireErrorCode::NotFound)
            );
        }
        assert!(fake.calls()?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn denied_list_and_worker_scope_do_not_call_engine()
    -> Result<(), Box<dyn std::error::Error>> {
        let (workflow_id, _run_id) = workflow_ids();
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(workflow_id, "tenant-b")?;
        let guard = guard_with_ownership(ownership);
        let fake = RecordingFakeEngine::new();
        let filter = WorkflowFilter::default();

        let list = ProtoListWorkflowsRequest {
            namespace: String::from("tenant-b"),
            filter: None,
        };
        let worker = ProtoRegisterWorker {
            namespaces: vec![String::from("tenant-b")],
            activity_types: vec![String::from("ship")],
            task_queue: String::new(),
            node: String::new(),
        };

        assert!(
            guard
                .scope(&caller(), &NamespaceOperation::list(&list, &filter))
                .await
                .is_err()
        );
        assert!(
            guard
                .scope(&caller(), &NamespaceOperation::register_worker(&worker),)
                .await
                .is_err()
        );
        assert!(fake.calls()?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn denied_subscriptions_do_not_call_engine() -> Result<(), Box<dyn std::error::Error>> {
        let (workflow_id, run_id) = workflow_ids();
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(workflow_id.clone(), "tenant-b")?;
        let guard = guard_with_ownership(ownership);
        let fake = RecordingFakeEngine::new();
        let event_filter = aion::EventFilter::default();

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
            resume_from_seq: None,
        };
        let cross_namespace_filter = aion::EventFilter {
            workflow_id: Some(workflow_id.clone()),
            run: None,
            family: None,
        };
        let firehose = FirehoseSubscription {
            namespace: String::from("tenant-b"),
        };

        let target = WorkflowTarget::with_run(&workflow_id, &run_id);
        // Namespace-grant and selector failures stay NamespaceDenied; a
        // workflow-targeted subscription that misses ownership in a granted
        // namespace is NotFound (anti-existence-leak).
        let denied_subscriptions = [
            (
                NamespaceOperation::subscribe(
                    SubscriptionScope::Filtered(&filtered),
                    &event_filter,
                ),
                aion_proto::WireErrorCode::NamespaceDenied,
            ),
            (
                NamespaceOperation::subscribe(
                    SubscriptionScope::Filtered(&filtered_by_workflow),
                    &cross_namespace_filter,
                ),
                aion_proto::WireErrorCode::NotFound,
            ),
            (
                NamespaceOperation::subscribe(
                    SubscriptionScope::PerWorkflow(&per_workflow, target),
                    &cross_namespace_filter,
                ),
                aion_proto::WireErrorCode::NotFound,
            ),
            (
                NamespaceOperation::subscribe(
                    SubscriptionScope::Firehose(&firehose),
                    &event_filter,
                ),
                aion_proto::WireErrorCode::NamespaceDenied,
            ),
        ];

        for (operation, expected_code) in &denied_subscriptions {
            assert_eq!(
                guard
                    .scope(&caller(), operation)
                    .await
                    .err()
                    .map(|error| error.to_wire_error().code)
                    .as_ref(),
                Some(expected_code)
            );
        }
        assert!(fake.calls()?.is_empty());
        Ok(())
    }

    fn schedule_id() -> ScheduleId {
        ScheduleId::new(uuid::Uuid::from_u128(9))
    }

    fn schedule_id_request(namespace: &str) -> ProtoScheduleIdRequest {
        ProtoScheduleIdRequest {
            namespace: namespace.to_owned(),
            schedule_id: None,
        }
    }

    #[tokio::test]
    async fn schedule_ownership_misses_are_not_found_and_do_not_call_engine()
    -> Result<(), Box<dyn std::error::Error>> {
        let schedule_id = schedule_id();
        let schedule_ownership = StaticScheduleNamespaces::default();
        schedule_ownership.record(schedule_id.clone(), "tenant-b")?;
        let counting = CountingScheduleNamespaces::wrapping(schedule_ownership);
        // The resolver carries no engine handle at all (`authorization_only`),
        // so every operation that errors here provably erred before any engine
        // access could exist.
        let guard = guard_with_schedule_ownership(counting.clone());
        let target = ScheduleTarget::schedule(&schedule_id);

        let update = ProtoUpdateScheduleRequest {
            namespace: String::from("tenant-a"),
            schedule_id: None,
            config: None,
        };
        let id_request = schedule_id_request("tenant-a");

        let operations = [
            NamespaceOperation::update_schedule(&update, target),
            NamespaceOperation::pause_schedule(&id_request, target),
            NamespaceOperation::resume_schedule(&id_request, target),
            NamespaceOperation::delete_schedule(&id_request, target),
            NamespaceOperation::describe_schedule(&id_request, target),
        ];
        let operation_count = operations.len();

        for operation in operations {
            let result = guard.scope(&caller(), &operation).await;
            // Ownership miss in a granted namespace is NotFound, not
            // NamespaceDenied: cross-tenant probes must be indistinguishable
            // from nonexistent schedules.
            let error = result
                .err()
                .map(|error| error.to_wire_error())
                .ok_or("expected foreign-owned schedule to be rejected")?;
            assert_eq!(error.code, aion_proto::WireErrorCode::NotFound);
            assert_eq!(error.message, "schedule not found in namespace tenant-a");
        }
        // Durable ownership was consulted exactly once per targeted operation:
        // the NotFound came from the verification step, not from skipping it.
        assert_eq!(counting.calls(), operation_count);
        Ok(())
    }

    #[tokio::test]
    async fn ungranted_schedule_operations_are_namespace_denied()
    -> Result<(), Box<dyn std::error::Error>> {
        let schedule_id = schedule_id();
        let schedule_ownership = StaticScheduleNamespaces::default();
        schedule_ownership.record(schedule_id.clone(), "tenant-b")?;
        let counting = CountingScheduleNamespaces::wrapping(schedule_ownership);
        let guard = guard_with_schedule_ownership(counting.clone());
        let target = ScheduleTarget::schedule(&schedule_id);

        let create = ProtoCreateScheduleRequest {
            namespace: String::from("tenant-b"),
            config: None,
        };
        let update = ProtoUpdateScheduleRequest {
            namespace: String::from("tenant-b"),
            schedule_id: None,
            config: None,
        };
        let id_request = schedule_id_request("tenant-b");
        let list = ProtoListSchedulesRequest {
            namespace: String::from("tenant-b"),
        };

        let operations = [
            NamespaceOperation::create_schedule(&create),
            NamespaceOperation::update_schedule(&update, target),
            NamespaceOperation::pause_schedule(&id_request, target),
            NamespaceOperation::resume_schedule(&id_request, target),
            NamespaceOperation::delete_schedule(&id_request, target),
            NamespaceOperation::describe_schedule(&id_request, target),
            NamespaceOperation::list_schedules(&list),
        ];

        // No grant for the requested namespace is NamespaceDenied for all
        // seven schedule operations — even when the target schedule really is
        // owned by that namespace, the grant check decides first.
        for operation in operations {
            let result = guard.scope(&caller(), &operation).await;
            assert_eq!(
                result.err().map(|error| error.to_wire_error().code),
                Some(aion_proto::WireErrorCode::NamespaceDenied)
            );
        }
        // The grant check short-circuits before target verification: durable
        // ownership must never be consulted for an ungranted namespace.
        assert_eq!(counting.calls(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn granted_schedule_create_and_list_return_scoped_engine()
    -> Result<(), Box<dyn std::error::Error>> {
        let guard = guard_with_schedule_ownership(StaticScheduleNamespaces::default());
        let create = ProtoCreateScheduleRequest {
            namespace: String::from("tenant-a"),
            config: None,
        };
        let list = ProtoListSchedulesRequest {
            namespace: String::from("tenant-a"),
        };

        let scoped_create = guard
            .scope(&caller(), &NamespaceOperation::create_schedule(&create))
            .await?;
        let scoped_list = guard
            .scope(&caller(), &NamespaceOperation::list_schedules(&list))
            .await?;

        assert_eq!(scoped_create.namespace(), "tenant-a");
        assert_eq!(scoped_list.namespace(), "tenant-a");
        Ok(())
    }

    #[tokio::test]
    async fn authorized_start_returns_scoped_engine() -> Result<(), Box<dyn std::error::Error>> {
        let guard = guard_with_ownership(StaticWorkflowNamespaces::default());
        let request = ProtoStartWorkflowRequest {
            namespace: String::from("tenant-a"),
            workflow_type: String::from("checkout"),
            input: None,
            routing_key: None,
            task_queue: None,
        };

        let scoped = guard
            .scope(&caller(), &NamespaceOperation::start(&request))
            .await?;

        assert_eq!(scoped.namespace(), "tenant-a");
        Ok(())
    }

    #[tokio::test]
    async fn single_tenant_mode_authorizes_configured_namespace()
    -> Result<(), Box<dyn std::error::Error>> {
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SingleTenant {
                namespace: String::from("tenant-a"),
            },
            StaticWorkflowNamespaces::default(),
            StaticScheduleNamespaces::default(),
        );
        let guard = NamespaceGuard::new(resolver);
        let request = ProtoRegisterWorker {
            namespaces: vec![String::from("tenant-a")],
            activity_types: Vec::new(),
            task_queue: String::new(),
            node: String::new(),
        };

        let scoped = guard
            .scope(
                &CallerIdentity::new("single-tenant", Vec::<String>::new()),
                &NamespaceOperation::register_worker(&request),
            )
            .await?;

        assert_eq!(scoped.namespace(), "tenant-a");
        Ok(())
    }
}
