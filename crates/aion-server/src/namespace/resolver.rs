//! Namespace resolver type wired into shared state.
//!
//! Workflow→namespace ownership is a projection of durable history: the server
//! records the owning namespace as the `aion.namespace` search attribute when a
//! workflow starts, and verification folds that attribute back out of the
//! workflow's recorded events. Nothing about ownership lives only in memory, so
//! a server restart can never orphan a workflow from its namespace.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use aion::Engine;
use aion_core::{ScheduleId, SearchAttributeValue, WorkflowId, search_attributes_from_events};
use aion_proto::WireError;
use async_trait::async_trait;

use crate::config::{NamespaceConfig, NamespaceMode};
use crate::error::ServerError;

use super::schedule_source::{HistoryScheduleNamespaceSource, ScheduleNamespaceSource};

/// Search attribute name that records the owning namespace of every workflow
/// started through this server.
pub const NAMESPACE_ATTRIBUTE: &str = "aion.namespace";

/// Where a caller's grants came from, so a denial message can point the
/// operator at the knob that actually carries the grant (the development
/// headers, or the validated token's claims).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GrantSource {
    /// Grants parsed from the development `x-aion-namespaces` /
    /// `x-aion-deploy` headers.
    NamespacesHeader,
    /// Grants carried by a validated token's claims.
    TokenClaim,
}

impl GrantSource {
    /// Stable label for audit log fields.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::NamespacesHeader => "header",
            Self::TokenClaim => "token_claim",
        }
    }
}

/// Authenticated caller metadata supplied by an adapter boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallerIdentity {
    subject: String,
    namespaces: BTreeSet<String>,
    denial_reason: Option<String>,
    grant_source: GrantSource,
    /// Whether the caller holds the deployment-wide deploy grant (the
    /// `deploy` token claim, or the `x-aion-deploy` development header).
    deploy: bool,
}

impl CallerIdentity {
    /// Build a caller identity whose namespace grants came from the
    /// development `x-aion-namespaces` header.
    #[must_use]
    pub fn new(subject: impl Into<String>, namespaces: impl IntoIterator<Item = String>) -> Self {
        Self {
            subject: subject.into(),
            namespaces: namespaces.into_iter().collect(),
            denial_reason: None,
            grant_source: GrantSource::NamespacesHeader,
            deploy: false,
        }
    }

    /// Build a caller identity whose namespace grants came from a validated
    /// token's namespace claim (the real JWT path), so denial messages direct
    /// the operator to the token grant instead of the development header.
    #[must_use]
    pub fn from_token_claims(
        subject: impl Into<String>,
        namespaces: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            subject: subject.into(),
            namespaces: namespaces.into_iter().collect(),
            denial_reason: None,
            grant_source: GrantSource::TokenClaim,
            deploy: false,
        }
    }

    /// Attach the deployment-wide deploy grant decision to this identity.
    ///
    /// The grant is engine-global, never namespace-scoped: loading a package
    /// re-points routing for a workflow type that is startable from every
    /// namespace, so a namespace-valued grant would promise an isolation the
    /// engine does not provide.
    #[must_use]
    pub fn with_deploy(mut self, deploy: bool) -> Self {
        self.deploy = deploy;
        self
    }

    /// Whether the caller holds the deployment-wide deploy grant.
    #[must_use]
    pub const fn deploy_granted(&self) -> bool {
        self.deploy
    }

    /// Build a caller identity that must be denied with a transport-specific reason.
    #[must_use]
    pub fn denied(subject: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            namespaces: BTreeSet::new(),
            denial_reason: Some(reason.into()),
            grant_source: GrantSource::NamespacesHeader,
            deploy: false,
        }
    }

    /// Caller subject as authenticated by the transport.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    fn can_access(&self, namespace: &str) -> bool {
        self.namespaces.contains(namespace)
    }

    pub(crate) fn denial_reason(&self) -> Option<&str> {
        self.denial_reason.as_deref()
    }

    /// Where this caller's grants came from, for grant-source-aware denials
    /// and audit fields.
    pub(crate) const fn grant_source(&self) -> GrantSource {
        self.grant_source
    }
}

/// Namespace-scoped access to the embedded engine.
#[derive(Clone)]
pub struct ScopedEngine {
    namespace: String,
    engine: Option<Arc<Engine>>,
}

impl ScopedEngine {
    /// Authorized namespace attached to this engine access.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Borrow the authorized engine handle for adapter code after guard approval.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Config`] only for resolver instances constructed
    /// without an engine for unit tests.
    pub fn engine(&self) -> Result<&Arc<Engine>, ServerError> {
        self.engine.as_ref().ok_or_else(|| ServerError::Config {
            message: "namespace resolver has no engine handle".to_owned(),
        })
    }
}

/// Durable per-workflow attribution facts projected from recorded history.
///
/// Namespace ownership and workflow type are both immutable projections of the
/// same durable history (ownership is recorded atomically with the
/// `WorkflowStarted` batch; the type is the most recent run's recorded
/// `WorkflowStarted` type), so one read serves both consumers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowAttribution {
    /// Namespace recorded as the workflow's owner.
    pub namespace: String,
    /// Workflow type recorded by the most recent `WorkflowStarted` event, or
    /// [`None`] when the history records no started run.
    pub workflow_type: Option<String>,
}

/// Durable source of workflow→namespace ownership and type attribution facts.
///
/// The production implementation projects attribution from recorded workflow
/// history; tests substitute a static fixture to prove adapter-boundary
/// denials without an engine.
#[async_trait]
pub trait WorkflowNamespaceSource: Send + Sync {
    /// Returns the attribution recorded for a workflow, or [`None`] when the
    /// workflow is unknown or recorded no namespace attribute.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when the underlying ownership data cannot be read.
    async fn workflow_attribution(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Option<WorkflowAttribution>, ServerError>;
}

/// Production attribution source: folds the `aion.namespace` search attribute
/// and the most recent `WorkflowStarted` type out of the workflow's durable
/// event history in a single read.
struct HistoryNamespaceSource {
    engine: Arc<Engine>,
}

#[async_trait]
impl WorkflowNamespaceSource for HistoryNamespaceSource {
    async fn workflow_attribution(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Option<WorkflowAttribution>, ServerError> {
        let history = self
            .engine
            .store()
            .read_history(workflow_id)
            .await
            .map_err(ServerError::from)?;
        let namespace = match search_attributes_from_events(&history).remove(NAMESPACE_ATTRIBUTE) {
            Some(SearchAttributeValue::String(namespace)) => namespace,
            Some(other) => {
                return Err(ServerError::Config {
                    message: format!(
                        "workflow {workflow_id} recorded a non-string {NAMESPACE_ATTRIBUTE} search attribute: {other:?}"
                    ),
                });
            }
            None => return Ok(None),
        };
        // Continue-as-new runs share one history; the most recent
        // `WorkflowStarted` carries the current run's workflow type.
        let workflow_type = history.iter().rev().find_map(|event| match event {
            aion_core::Event::WorkflowStarted { workflow_type, .. } => Some(workflow_type.clone()),
            _ => None,
        });
        Ok(Some(WorkflowAttribution {
            namespace,
            workflow_type,
        }))
    }
}

/// Static workflow→namespace fixture for adapter-boundary tests and alternate
/// wiring that must authorize without an engine handle.
#[derive(Clone, Default)]
pub struct StaticWorkflowNamespaces {
    inner: Arc<RwLock<HashMap<WorkflowId, WorkflowAttribution>>>,
}

impl StaticWorkflowNamespaces {
    /// Record that a workflow is owned by a namespace, with no recorded
    /// workflow type (the fixture equivalent of a history without a
    /// `WorkflowStarted` event).
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the fixture lock was poisoned.
    pub fn record(&self, workflow_id: WorkflowId, namespace: &str) -> Result<(), ServerError> {
        self.insert(
            workflow_id,
            WorkflowAttribution {
                namespace: namespace.to_owned(),
                workflow_type: None,
            },
        )
    }

    /// Record that a workflow is owned by a namespace and carries a recorded
    /// workflow type.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the fixture lock was poisoned.
    pub fn record_with_type(
        &self,
        workflow_id: WorkflowId,
        namespace: &str,
        workflow_type: &str,
    ) -> Result<(), ServerError> {
        self.insert(
            workflow_id,
            WorkflowAttribution {
                namespace: namespace.to_owned(),
                workflow_type: Some(workflow_type.to_owned()),
            },
        )
    }

    fn insert(
        &self,
        workflow_id: WorkflowId,
        attribution: WorkflowAttribution,
    ) -> Result<(), ServerError> {
        let mut ownership = self
            .inner
            .write()
            .map_err(|_| ServerError::lock_poisoned("namespace workflow ownership"))?;
        ownership.insert(workflow_id, attribution);
        Ok(())
    }
}

#[async_trait]
impl WorkflowNamespaceSource for StaticWorkflowNamespaces {
    async fn workflow_attribution(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Option<WorkflowAttribution>, ServerError> {
        let ownership = self
            .inner
            .read()
            .map_err(|_| ServerError::lock_poisoned("namespace workflow ownership"))?;
        Ok(ownership.get(workflow_id).cloned())
    }
}

/// Resolver that authorizes callers and yields namespace-scoped engine access.
#[derive(Clone)]
pub struct NamespaceResolver {
    mode: NamespaceMode,
    engine: Option<Arc<Engine>>,
    ownership: Arc<dyn WorkflowNamespaceSource>,
    schedule_ownership: Arc<dyn ScheduleNamespaceSource>,
}

impl NamespaceResolver {
    /// Build a resolver from operator-supplied namespace configuration and the
    /// engine selected for this deployment.
    #[must_use]
    pub fn from_config(config: NamespaceConfig, engine: Arc<Engine>) -> Self {
        Self {
            mode: config.mode,
            ownership: Arc::new(HistoryNamespaceSource {
                engine: Arc::clone(&engine),
            }),
            schedule_ownership: Arc::new(HistoryScheduleNamespaceSource::new(Arc::clone(&engine))),
            engine: Some(engine),
        }
    }

    /// Build a resolver from explicit parts for tests and alternate wiring.
    #[must_use]
    pub fn from_parts(
        mode: NamespaceMode,
        engine: Option<Arc<Engine>>,
        ownership: Arc<dyn WorkflowNamespaceSource>,
        schedule_ownership: Arc<dyn ScheduleNamespaceSource>,
    ) -> Self {
        Self {
            mode,
            engine,
            ownership,
            schedule_ownership,
        }
    }

    /// Build a resolver that performs authorization and ownership checks only.
    ///
    /// This constructor is intended for adapter-boundary unit tests that must
    /// prove denied operations do not reach any engine handle.
    #[must_use]
    pub fn authorization_only(
        mode: NamespaceMode,
        ownership: impl WorkflowNamespaceSource + 'static,
        schedule_ownership: impl ScheduleNamespaceSource + 'static,
    ) -> Self {
        Self::from_parts(
            mode,
            None,
            Arc::new(ownership),
            Arc::new(schedule_ownership),
        )
    }

    /// Inspect the configured namespace mode.
    #[must_use]
    pub const fn mode(&self) -> &NamespaceMode {
        &self.mode
    }

    /// Borrow the engine handle for engine-global (non-namespace) operations.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Config`] only for resolver instances constructed
    /// without an engine for unit tests.
    pub(crate) fn engine(&self) -> Result<&Arc<Engine>, ServerError> {
        self.engine.as_ref().ok_or_else(|| ServerError::Config {
            message: "namespace resolver has no engine handle".to_owned(),
        })
    }

    /// Shut down the engine owned by this resolver.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Config`] when no engine is attached, or [`ServerError::EngineCall`]
    /// when the engine rejects shutdown.
    pub fn shutdown_engine(&self) -> Result<(), ServerError> {
        self.engine
            .as_ref()
            .ok_or_else(|| ServerError::Config {
                message: "namespace resolver has no engine handle".to_owned(),
            })?
            .shutdown()
            .map_err(ServerError::from)
    }

    /// Authorize a caller for a requested namespace and return scoped engine
    /// access if allowed.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Namespace`] when the caller is not authorized for
    /// the namespace selected by the wire request.
    pub(super) fn resolve(
        &self,
        caller: &CallerIdentity,
        requested_namespace: &str,
    ) -> Result<ScopedEngine, ServerError> {
        if requested_namespace.is_empty() {
            return Err(ServerError::namespace_denied(
                "requested namespace must not be empty",
            ));
        }

        if let Some(reason) = caller.denial_reason() {
            return Err(ServerError::namespace_denied(reason));
        }

        match &self.mode {
            NamespaceMode::SingleTenant { namespace } if namespace == requested_namespace => {
                Ok(self.scoped(requested_namespace))
            }
            NamespaceMode::SharedEngine if caller.can_access(requested_namespace) => {
                Ok(self.scoped(requested_namespace))
            }
            NamespaceMode::SingleTenant { .. } | NamespaceMode::SharedEngine => {
                Err(namespace_denied(caller, requested_namespace))
            }
        }
    }

    /// Verify durable workflow ownership against the requested namespace.
    ///
    /// `NamespaceDenied` means exactly one thing: the caller has no grant for
    /// the requested namespace, and that is decided by [`Self::resolve`] before
    /// this check runs. Workflow-level visibility misses are `NotFound` to
    /// prevent existence leaks: when the caller's requested namespace is
    /// granted but the workflow's recorded owner namespace is absent (unknown
    /// workflow, or no recorded attribute) or different (owned by another
    /// tenant), both cases return the identical `not_found` wire error with
    /// the identical message, so a cross-tenant probe is byte-for-byte
    /// indistinguishable from querying a workflow that never existed.
    ///
    /// # Errors
    ///
    /// Returns a [`ServerError::Wire`] `not_found` error when the workflow is
    /// not visible in the requested namespace; ownership-source read failures
    /// surface as their own typed errors.
    pub async fn verify_workflow_ownership(
        &self,
        namespace: &str,
        workflow_id: &WorkflowId,
    ) -> Result<(), ServerError> {
        match self.workflow_attribution(namespace, workflow_id).await? {
            Some(_) => Ok(()),
            None => Err(ServerError::Wire {
                wire: WireError::not_found(format!("workflow not found in namespace {namespace}")),
            }),
        }
    }

    /// Read a workflow's durable attribution scoped to one namespace.
    ///
    /// Returns the recorded attribution only when the workflow's recorded
    /// owner namespace equals `namespace`. Foreign-owned and unknown workflows
    /// both yield [`None`] (anti-existence-leak: callers must treat the two
    /// cases identically and never disclose which one occurred).
    ///
    /// This is the single read that serves both the namespace verdict and the
    /// workflow-type lookup at the streaming seam — one durable history read
    /// per workflow answers both questions.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when the underlying ownership data cannot be
    /// read; callers must fail loudly rather than guessing.
    pub async fn workflow_attribution(
        &self,
        namespace: &str,
        workflow_id: &WorkflowId,
    ) -> Result<Option<WorkflowAttribution>, ServerError> {
        Ok(self
            .ownership
            .workflow_attribution(workflow_id)
            .await?
            .filter(|attribution| attribution.namespace == namespace))
    }

    /// Verify durable schedule ownership against the requested namespace.
    ///
    /// `NamespaceDenied` means exactly one thing: the caller has no grant for
    /// the requested namespace, and that is decided by [`Self::resolve`] before
    /// this check runs. Schedule-level visibility misses are `NotFound` to
    /// prevent existence leaks: when the caller's requested namespace is
    /// granted but the schedule's creation-recorded owner namespace is absent
    /// (unknown schedule, or no recorded attribute) or different (owned by
    /// another tenant), both cases return the identical `not_found` wire error
    /// with the identical message, so a cross-tenant probe is byte-for-byte
    /// indistinguishable from targeting a schedule that never existed.
    ///
    /// # Errors
    ///
    /// Returns a [`ServerError::Wire`] `not_found` error when the schedule is
    /// not visible in the requested namespace; ownership-source read failures
    /// surface as their own typed errors.
    pub async fn verify_schedule_ownership(
        &self,
        namespace: &str,
        schedule_id: &ScheduleId,
    ) -> Result<(), ServerError> {
        match self
            .schedule_ownership
            .schedule_namespace(schedule_id)
            .await?
        {
            Some(owner) if owner == namespace => Ok(()),
            // Anti-existence-leak: absent and foreign ownership must be one
            // identical NotFound, never a distinguishable denial.
            Some(_) | None => Err(ServerError::Wire {
                wire: WireError::not_found(format!("schedule not found in namespace {namespace}")),
            }),
        }
    }

    fn scoped(&self, namespace: &str) -> ScopedEngine {
        ScopedEngine {
            namespace: namespace.to_owned(),
            engine: self.engine.clone(),
        }
    }
}

fn namespace_denied(caller: &CallerIdentity, requested_namespace: &str) -> ServerError {
    let hint = match caller.grant_source {
        GrantSource::NamespacesHeader => format!(
            "add {requested_namespace} to x-aion-namespaces for subject `{}` or request a namespace listed in that header",
            caller.subject()
        ),
        GrantSource::TokenClaim => format!(
            "grant {requested_namespace} in the namespace claim of the token minted for subject `{}` or request a namespace the token grants",
            caller.subject()
        ),
    };
    ServerError::namespace_denied(format!(
        "subject not authorized for namespace {requested_namespace}; {hint}"
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        CallerIdentity, NamespaceResolver, StaticWorkflowNamespaces, WorkflowNamespaceSource,
    };
    use crate::config::NamespaceMode;
    use crate::namespace::StaticScheduleNamespaces;
    use aion_core::{ScheduleId, WorkflowId};

    fn resolver(mode: NamespaceMode) -> NamespaceResolver {
        NamespaceResolver::authorization_only(
            mode,
            StaticWorkflowNamespaces::default(),
            StaticScheduleNamespaces::default(),
        )
    }

    #[test]
    fn shared_engine_authorizes_explicit_caller_grant() -> Result<(), Box<dyn std::error::Error>> {
        let resolver = resolver(NamespaceMode::SharedEngine);
        let caller = CallerIdentity::new("alice", [String::from("tenant-a")]);

        let scoped = resolver.resolve(&caller, "tenant-a")?;

        assert_eq!(scoped.namespace(), "tenant-a");
        Ok(())
    }

    #[test]
    fn shared_engine_denies_missing_caller_grant() {
        let resolver = resolver(NamespaceMode::SharedEngine);
        let caller = CallerIdentity::new("alice", [String::from("tenant-a")]);

        let denied = resolver.resolve(&caller, "tenant-b");

        assert!(denied.is_err());
    }

    #[test]
    fn single_tenant_authorizes_only_configured_namespace() -> Result<(), Box<dyn std::error::Error>>
    {
        let resolver = resolver(NamespaceMode::SingleTenant {
            namespace: String::from("tenant-a"),
        });
        let caller = CallerIdentity::new("alice", [String::from("tenant-b")]);

        let scoped = resolver.resolve(&caller, "tenant-a")?;
        let denied = resolver.resolve(&caller, "tenant-b");

        assert_eq!(scoped.namespace(), "tenant-a");
        assert!(denied.is_err());
        Ok(())
    }

    /// The denial hint must point at the knob that actually carries the
    /// caller's grants: the development `x-aion-namespaces` header for
    /// header-sourced identities, the token's namespace claim for identities
    /// produced by the JWT path.
    #[test]
    fn denial_hint_names_the_grant_source() -> Result<(), Box<dyn std::error::Error>> {
        let resolver = resolver(NamespaceMode::SharedEngine);

        let header_caller = CallerIdentity::new("alice", [String::from("tenant-a")]);
        let header_denial = resolver
            .resolve(&header_caller, "tenant-b")
            .err()
            .map(|error| error.to_wire_error())
            .ok_or("expected header-sourced caller to be denied")?;
        assert!(
            header_denial.message.contains("x-aion-namespaces"),
            "header-path denial must hint the dev header: {}",
            header_denial.message
        );
        assert!(
            !header_denial.message.contains("namespace claim"),
            "header-path denial must not hint the token claim: {}",
            header_denial.message
        );

        let token_caller = CallerIdentity::from_token_claims("alice", [String::from("tenant-a")]);
        let token_denial = resolver
            .resolve(&token_caller, "tenant-b")
            .err()
            .map(|error| error.to_wire_error())
            .ok_or("expected token-sourced caller to be denied")?;
        assert!(
            token_denial.message.contains("namespace claim"),
            "JWT-path denial must hint the token's namespace claim: {}",
            token_denial.message
        );
        assert!(
            !token_denial.message.contains("x-aion-namespaces"),
            "JWT-path denial must not hint the dev header: {}",
            token_denial.message
        );
        Ok(())
    }

    #[test]
    fn empty_namespace_is_denied_before_scoping() {
        let resolver = resolver(NamespaceMode::SharedEngine);
        let caller = CallerIdentity::new("alice", [String::new()]);

        let denied = resolver.resolve(&caller, "");

        assert!(denied.is_err());
    }

    #[tokio::test]
    async fn ownership_misses_are_indistinguishable_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let ownership = StaticWorkflowNamespaces::default();
        let owned = WorkflowId::new(uuid::Uuid::from_u128(1));
        let unknown = WorkflowId::new(uuid::Uuid::from_u128(2));
        ownership.record(owned.clone(), "tenant-a")?;
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            ownership,
            StaticScheduleNamespaces::default(),
        );

        resolver
            .verify_workflow_ownership("tenant-a", &owned)
            .await?;

        // Foreign-owned and nonexistent workflows must produce byte-for-byte
        // identical NotFound wire errors (anti-existence-leak), never
        // NamespaceDenied.
        let foreign = resolver
            .verify_workflow_ownership("tenant-b", &owned)
            .await
            .err()
            .map(|error| error.to_wire_error())
            .ok_or("expected foreign-owned workflow to be rejected")?;
        let absent = resolver
            .verify_workflow_ownership("tenant-b", &unknown)
            .await
            .err()
            .map(|error| error.to_wire_error())
            .ok_or("expected unknown workflow to be rejected")?;

        assert_eq!(foreign.code, aion_proto::WireErrorCode::NotFound);
        assert_eq!(foreign, absent);
        assert_eq!(foreign.message, "workflow not found in namespace tenant-b");

        let absent_in_granted = resolver
            .verify_workflow_ownership("tenant-a", &unknown)
            .await
            .err()
            .map(|error| error.to_wire_error())
            .ok_or("expected unknown workflow to be rejected in granted namespace")?;
        assert_eq!(absent_in_granted.code, aion_proto::WireErrorCode::NotFound);
        assert_eq!(
            absent_in_granted.message,
            "workflow not found in namespace tenant-a"
        );
        Ok(())
    }

    #[tokio::test]
    async fn schedule_ownership_misses_are_indistinguishable_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let schedule_ownership = StaticScheduleNamespaces::default();
        let owned = ScheduleId::new(uuid::Uuid::from_u128(1));
        let unknown = ScheduleId::new(uuid::Uuid::from_u128(2));
        schedule_ownership.record(owned.clone(), "tenant-a")?;
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            StaticWorkflowNamespaces::default(),
            schedule_ownership,
        );

        resolver
            .verify_schedule_ownership("tenant-a", &owned)
            .await?;

        // Foreign-owned and nonexistent schedules must produce byte-for-byte
        // identical NotFound wire errors (anti-existence-leak), never
        // NamespaceDenied.
        let foreign = resolver
            .verify_schedule_ownership("tenant-b", &owned)
            .await
            .err()
            .map(|error| error.to_wire_error())
            .ok_or("expected foreign-owned schedule to be rejected")?;
        let absent = resolver
            .verify_schedule_ownership("tenant-b", &unknown)
            .await
            .err()
            .map(|error| error.to_wire_error())
            .ok_or("expected unknown schedule to be rejected")?;

        assert_eq!(foreign.code, aion_proto::WireErrorCode::NotFound);
        assert_eq!(foreign, absent);
        assert_eq!(foreign.message, "schedule not found in namespace tenant-b");

        let absent_in_granted = resolver
            .verify_schedule_ownership("tenant-a", &unknown)
            .await
            .err()
            .map(|error| error.to_wire_error())
            .ok_or("expected unknown schedule to be rejected in granted namespace")?;
        assert_eq!(absent_in_granted.code, aion_proto::WireErrorCode::NotFound);
        assert_eq!(
            absent_in_granted.message,
            "schedule not found in namespace tenant-a"
        );
        Ok(())
    }

    #[tokio::test]
    async fn static_source_reports_recorded_namespace() -> Result<(), Box<dyn std::error::Error>> {
        let ownership = StaticWorkflowNamespaces::default();
        let workflow_id = WorkflowId::new(uuid::Uuid::from_u128(3));
        ownership.record(workflow_id.clone(), "tenant-a")?;

        assert_eq!(
            ownership.workflow_attribution(&workflow_id).await?,
            Some(super::WorkflowAttribution {
                namespace: String::from("tenant-a"),
                workflow_type: None,
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn static_source_reports_recorded_workflow_type() -> Result<(), Box<dyn std::error::Error>>
    {
        let ownership = StaticWorkflowNamespaces::default();
        let workflow_id = WorkflowId::new(uuid::Uuid::from_u128(4));
        ownership.record_with_type(workflow_id.clone(), "tenant-a", "checkout")?;

        assert_eq!(
            ownership.workflow_attribution(&workflow_id).await?,
            Some(super::WorkflowAttribution {
                namespace: String::from("tenant-a"),
                workflow_type: Some(String::from("checkout")),
            })
        );
        Ok(())
    }

    /// The namespace-scoped attribution read must hide foreign and unknown
    /// workflows identically (anti-existence-leak) while exposing the recorded
    /// type for owned workflows.
    #[tokio::test]
    async fn scoped_attribution_hides_foreign_and_unknown_identically()
    -> Result<(), Box<dyn std::error::Error>> {
        let ownership = StaticWorkflowNamespaces::default();
        let owned = WorkflowId::new(uuid::Uuid::from_u128(5));
        let foreign = WorkflowId::new(uuid::Uuid::from_u128(6));
        let unknown = WorkflowId::new(uuid::Uuid::from_u128(7));
        ownership.record_with_type(owned.clone(), "tenant-a", "checkout")?;
        ownership.record_with_type(foreign.clone(), "tenant-b", "checkout")?;
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            ownership,
            StaticScheduleNamespaces::default(),
        );

        let visible = resolver
            .workflow_attribution("tenant-a", &owned)
            .await?
            .ok_or("owned workflow attribution must be visible")?;
        assert_eq!(visible.workflow_type.as_deref(), Some("checkout"));
        assert_eq!(
            resolver.workflow_attribution("tenant-a", &foreign).await?,
            None
        );
        assert_eq!(
            resolver.workflow_attribution("tenant-a", &unknown).await?,
            None
        );
        Ok(())
    }
}
