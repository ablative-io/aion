//! Namespace resolver type wired into shared state.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use aion::Engine;
use aion_core::WorkflowId;

use crate::config::{NamespaceConfig, NamespaceMode};
use crate::error::ServerError;

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
    ownership: WorkflowOwnership,
}

impl ScopedEngine {
    /// Authorized namespace attached to this engine access.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Record ownership for a workflow started in this namespace.
    ///
    /// This is server-side namespace metadata, not workflow execution logic. It is
    /// used by later adapter handlers to reject target operations before they can
    /// reach the engine.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the ownership registry lock was
    /// poisoned.
    pub fn record_workflow(&self, workflow_id: WorkflowId) -> Result<(), ServerError> {
        self.ownership.record(workflow_id, &self.namespace)
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

/// Resolver that authorizes callers and yields namespace-scoped engine access.
#[derive(Clone)]
pub struct NamespaceResolver {
    mode: NamespaceMode,
    engine: Option<Arc<Engine>>,
    ownership: WorkflowOwnership,
}

impl NamespaceResolver {
    /// Build a resolver from operator-supplied namespace configuration and the
    /// engine selected for this deployment.
    #[must_use]
    pub fn from_config(config: NamespaceConfig, engine: Arc<Engine>) -> Self {
        Self {
            mode: config.mode,
            engine: Some(engine),
            ownership: WorkflowOwnership::default(),
        }
    }

    /// Build a resolver from explicit parts for tests and alternate wiring.
    #[must_use]
    pub fn from_parts(
        mode: NamespaceMode,
        engine: Option<Arc<Engine>>,
        ownership: WorkflowOwnership,
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
    pub fn authorization_only(mode: NamespaceMode, ownership: WorkflowOwnership) -> Self {
        Self::from_parts(mode, None, ownership)
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

    /// Verify server-side workflow ownership without calling the engine.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Namespace`] when the workflow is not known to be in
    /// the authorized namespace, and [`ServerError::LockPoisoned`] if the
    /// ownership registry lock was poisoned.
    pub fn verify_workflow_ownership(
        &self,
        namespace: &str,
        workflow_id: &WorkflowId,
    ) -> Result<(), ServerError> {
        self.ownership.verify(namespace, workflow_id)
    }

    fn scoped(&self, namespace: &str) -> ScopedEngine {
        ScopedEngine {
            namespace: namespace.to_owned(),
            engine: self.engine.clone(),
            ownership: self.ownership.clone(),
        }
    }
}

fn namespace_denied(caller: &CallerIdentity, requested_namespace: &str) -> ServerError {
    ServerError::namespace_denied(format!(
        "subject not authorized for namespace {requested_namespace}; add {requested_namespace} to x-aion-namespaces for subject `{}` or request a namespace listed in that header",
        caller.subject()
    ))
}

/// Server-side workflow namespace ownership metadata.
#[derive(Clone, Default)]
pub struct WorkflowOwnership {
    inner: Arc<RwLock<HashMap<WorkflowId, String>>>,
}

impl WorkflowOwnership {
    /// Record that a workflow is owned by a namespace.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the ownership registry lock was
    /// poisoned.
    pub fn record(&self, workflow_id: WorkflowId, namespace: &str) -> Result<(), ServerError> {
        let mut ownership = self
            .inner
            .write()
            .map_err(|_| ServerError::lock_poisoned("namespace workflow ownership"))?;
        ownership.insert(workflow_id, namespace.to_owned());
        Ok(())
    }

    /// Verify that a workflow belongs to the requested namespace.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Namespace`] when the workflow is unknown or belongs
    /// to a different namespace, and [`ServerError::LockPoisoned`] if the
    /// ownership registry lock was poisoned.
    pub fn verify(&self, namespace: &str, workflow_id: &WorkflowId) -> Result<(), ServerError> {
        let ownership = self
            .inner
            .read()
            .map_err(|_| ServerError::lock_poisoned("namespace workflow ownership"))?;
        match ownership.get(workflow_id) {
            Some(owner) if owner == namespace => Ok(()),
            Some(_) | None => Err(ServerError::namespace_denied(
                "workflow is not visible in requested namespace",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CallerIdentity, NamespaceResolver, WorkflowOwnership};
    use crate::config::NamespaceMode;

    fn resolver(mode: NamespaceMode) -> NamespaceResolver {
        NamespaceResolver::authorization_only(mode, WorkflowOwnership::default())
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
}
