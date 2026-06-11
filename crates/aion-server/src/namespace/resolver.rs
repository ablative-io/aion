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
use aion_core::{SearchAttributeValue, WorkflowId, search_attributes_from_events};
use aion_proto::WireError;
use async_trait::async_trait;

use crate::config::{NamespaceConfig, NamespaceMode};
use crate::error::ServerError;

/// Search attribute name that records the owning namespace of every workflow
/// started through this server.
pub const NAMESPACE_ATTRIBUTE: &str = "aion.namespace";

/// Authenticated caller metadata supplied by an adapter boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallerIdentity {
    subject: String,
    namespaces: BTreeSet<String>,
    denial_reason: Option<String>,
}

impl CallerIdentity {
    /// Build a caller identity with explicit namespace grants.
    #[must_use]
    pub fn new(subject: impl Into<String>, namespaces: impl IntoIterator<Item = String>) -> Self {
        Self {
            subject: subject.into(),
            namespaces: namespaces.into_iter().collect(),
            denial_reason: None,
        }
    }

    /// Build a caller identity that must be denied with a transport-specific reason.
    #[must_use]
    pub fn denied(subject: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            namespaces: BTreeSet::new(),
            denial_reason: Some(reason.into()),
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

    fn denial_reason(&self) -> Option<&str> {
        self.denial_reason.as_deref()
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

/// Durable source of workflow→namespace ownership facts.
///
/// The production implementation projects ownership from recorded workflow
/// history; tests substitute a static fixture to prove adapter-boundary
/// denials without an engine.
#[async_trait]
pub trait WorkflowNamespaceSource: Send + Sync {
    /// Returns the namespace recorded for a workflow, or [`None`] when the
    /// workflow is unknown or recorded no namespace attribute.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when the underlying ownership data cannot be read.
    async fn workflow_namespace(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Option<String>, ServerError>;
}

/// Production ownership source: folds the `aion.namespace` search attribute
/// out of the workflow's durable event history.
struct HistoryNamespaceSource {
    engine: Arc<Engine>,
}

#[async_trait]
impl WorkflowNamespaceSource for HistoryNamespaceSource {
    async fn workflow_namespace(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Option<String>, ServerError> {
        let history = self
            .engine
            .store()
            .read_history(workflow_id)
            .await
            .map_err(ServerError::from)?;
        match search_attributes_from_events(&history).remove(NAMESPACE_ATTRIBUTE) {
            Some(SearchAttributeValue::String(namespace)) => Ok(Some(namespace)),
            Some(other) => Err(ServerError::Config {
                message: format!(
                    "workflow {workflow_id} recorded a non-string {NAMESPACE_ATTRIBUTE} search attribute: {other:?}"
                ),
            }),
            None => Ok(None),
        }
    }
}

/// Static workflow→namespace fixture for adapter-boundary tests and alternate
/// wiring that must authorize without an engine handle.
#[derive(Clone, Default)]
pub struct StaticWorkflowNamespaces {
    inner: Arc<RwLock<HashMap<WorkflowId, String>>>,
}

impl StaticWorkflowNamespaces {
    /// Record that a workflow is owned by a namespace.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the fixture lock was poisoned.
    pub fn record(&self, workflow_id: WorkflowId, namespace: &str) -> Result<(), ServerError> {
        let mut ownership = self
            .inner
            .write()
            .map_err(|_| ServerError::lock_poisoned("namespace workflow ownership"))?;
        ownership.insert(workflow_id, namespace.to_owned());
        Ok(())
    }
}

#[async_trait]
impl WorkflowNamespaceSource for StaticWorkflowNamespaces {
    async fn workflow_namespace(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Option<String>, ServerError> {
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
            engine: Some(engine),
        }
    }

    /// Build a resolver from explicit parts for tests and alternate wiring.
    #[must_use]
    pub fn from_parts(
        mode: NamespaceMode,
        engine: Option<Arc<Engine>>,
        ownership: Arc<dyn WorkflowNamespaceSource>,
    ) -> Self {
        Self {
            mode,
            engine,
            ownership,
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
    ) -> Self {
        Self::from_parts(mode, None, Arc::new(ownership))
    }

    /// Inspect the configured namespace mode.
    #[must_use]
    pub const fn mode(&self) -> &NamespaceMode {
        &self.mode
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
        match self.ownership.workflow_namespace(workflow_id).await? {
            Some(owner) if owner == namespace => Ok(()),
            // Anti-existence-leak: absent and foreign ownership must be one
            // identical NotFound, never a distinguishable denial.
            Some(_) | None => Err(ServerError::Wire {
                wire: WireError::not_found(format!("workflow not found in namespace {namespace}")),
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
    ServerError::namespace_denied(format!(
        "subject not authorized for namespace {requested_namespace}; add {requested_namespace} to x-aion-namespaces for subject `{}` or request a namespace listed in that header",
        caller.subject()
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        CallerIdentity, NamespaceResolver, StaticWorkflowNamespaces, WorkflowNamespaceSource,
    };
    use crate::config::NamespaceMode;
    use aion_core::WorkflowId;

    fn resolver(mode: NamespaceMode) -> NamespaceResolver {
        NamespaceResolver::authorization_only(mode, StaticWorkflowNamespaces::default())
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
        let resolver =
            NamespaceResolver::authorization_only(NamespaceMode::SharedEngine, ownership);

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
    async fn static_source_reports_recorded_namespace() -> Result<(), Box<dyn std::error::Error>> {
        let ownership = StaticWorkflowNamespaces::default();
        let workflow_id = WorkflowId::new(uuid::Uuid::from_u128(3));
        ownership.record(workflow_id.clone(), "tenant-a")?;

        assert_eq!(
            ownership.workflow_namespace(&workflow_id).await?,
            Some(String::from("tenant-a"))
        );
        Ok(())
    }
}
