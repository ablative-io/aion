//! Namespace-aware event gating at the broadcast/encode seam.
//!
//! The engine's broadcast channel is engine-global and `EventFilter` has no
//! namespace dimension, while one shared `Engine` serves every tenant. Every
//! subscription kind — per-workflow, filtered, and firehose — must therefore
//! pass each live event through this gate before a frame is encoded, so a
//! tenant's socket can never receive (or be labeled with) another tenant's
//! events.

use std::collections::HashMap;

use aion_core::{Event, WorkflowId};
use aion_proto::WireErrorCode;

use crate::error::ServerError;
use crate::namespace::NamespaceResolver;

/// Per-connection gate deciding whether an event belongs to the authorized
/// namespace.
///
/// Verdicts are cached per workflow for the connection's lifetime. The cache
/// is sound because a workflow's owner namespace is recorded atomically with
/// its `WorkflowStarted` batch and never changes, and the publisher broadcasts
/// only after durable commit — so by the time any event for a workflow is
/// observed here, its ownership verdict is durable and final.
pub struct NamespaceEventGate {
    resolver: NamespaceResolver,
    namespace: String,
    verdicts: HashMap<WorkflowId, bool>,
}

impl NamespaceEventGate {
    /// Build a gate for one authorized namespace.
    #[must_use]
    pub fn new(resolver: NamespaceResolver, namespace: String) -> Self {
        Self {
            resolver,
            namespace,
            verdicts: HashMap::new(),
        }
    }

    /// Pre-seed an allow verdict for a workflow whose ownership the namespace
    /// guard already verified (the per-workflow subscription target), so the
    /// hot path never re-reads history for it.
    pub fn allow(&mut self, workflow_id: WorkflowId) {
        self.verdicts.insert(workflow_id, true);
    }

    /// Decide whether `event` may be delivered to this connection.
    ///
    /// `Ok(false)` means the event's workflow is foreign or unknown to the
    /// authorized namespace and must be silently filtered out (a firehose has
    /// no entitlement to learn that foreign events exist at all).
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when the durable ownership source cannot be
    /// read; callers must terminate the stream loudly rather than guessing.
    pub async fn permits(&mut self, event: &Event) -> Result<bool, ServerError> {
        if let Some(verdict) = self.verdicts.get(event.workflow_id()) {
            return Ok(*verdict);
        }
        let verdict = match self
            .resolver
            .verify_workflow_ownership(&self.namespace, event.workflow_id())
            .await
        {
            Ok(()) => true,
            // The guard's deliberate anti-existence-leak NotFound covers both
            // foreign-owned and unknown workflows: neither may be delivered.
            Err(error) if error.to_wire_error().code == WireErrorCode::NotFound => false,
            Err(error) => return Err(error),
        };
        self.verdicts.insert(event.workflow_id().clone(), verdict);
        Ok(verdict)
    }
}

#[cfg(test)]
mod tests {
    use aion_core::{Event, EventEnvelope, Payload, WorkflowId};
    use async_trait::async_trait;

    use super::NamespaceEventGate;
    use crate::config::NamespaceMode;
    use crate::error::ServerError;
    use crate::namespace::{
        NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces,
        WorkflowNamespaceSource,
    };

    fn event(seq: u64, workflow_id: &WorkflowId) -> Result<Event, aion_core::PayloadError> {
        Ok(Event::SignalReceived {
            envelope: EventEnvelope {
                seq,
                recorded_at: chrono::Utc::now(),
                workflow_id: workflow_id.clone(),
            },
            name: "ship".to_owned(),
            payload: Payload::from_json(&serde_json::json!({ "seq": seq }))?,
        })
    }

    fn resolver(ownership: StaticWorkflowNamespaces) -> NamespaceResolver {
        NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            ownership,
            StaticScheduleNamespaces::default(),
        )
    }

    #[tokio::test]
    async fn gate_permits_own_namespace_and_filters_foreign_and_unknown()
    -> Result<(), Box<dyn std::error::Error>> {
        let own = WorkflowId::new(uuid::Uuid::from_u128(1));
        let foreign = WorkflowId::new(uuid::Uuid::from_u128(2));
        let unknown = WorkflowId::new(uuid::Uuid::from_u128(3));
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(own.clone(), "tenant-a")?;
        ownership.record(foreign.clone(), "tenant-b")?;
        let mut gate = NamespaceEventGate::new(resolver(ownership), "tenant-a".to_owned());

        assert!(gate.permits(&event(1, &own)?).await?);
        assert!(!gate.permits(&event(1, &foreign)?).await?);
        assert!(!gate.permits(&event(1, &unknown)?).await?);
        Ok(())
    }

    /// Ownership source that fails after a configurable number of reads so the
    /// cache and the loud-failure path can both be proven.
    struct CountingOwnership {
        inner: StaticWorkflowNamespaces,
        reads: std::sync::atomic::AtomicUsize,
        fail_after: usize,
    }

    #[async_trait]
    impl WorkflowNamespaceSource for CountingOwnership {
        async fn workflow_namespace(
            &self,
            workflow_id: &WorkflowId,
        ) -> Result<Option<String>, ServerError> {
            let reads = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if reads >= self.fail_after {
                return Err(ServerError::Config {
                    message: "ownership source unavailable".to_owned(),
                });
            }
            self.inner.workflow_namespace(workflow_id).await
        }
    }

    #[tokio::test]
    async fn verdicts_are_cached_per_workflow() -> Result<(), Box<dyn std::error::Error>> {
        let own = WorkflowId::new(uuid::Uuid::from_u128(1));
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(own.clone(), "tenant-a")?;
        let counting = CountingOwnership {
            inner: ownership,
            reads: std::sync::atomic::AtomicUsize::new(0),
            fail_after: 1,
        };
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            counting,
            StaticScheduleNamespaces::default(),
        );
        let mut gate = NamespaceEventGate::new(resolver, "tenant-a".to_owned());

        // Second permits() must hit the cache; a second source read would fail.
        assert!(gate.permits(&event(1, &own)?).await?);
        assert!(gate.permits(&event(2, &own)?).await?);
        Ok(())
    }

    #[tokio::test]
    async fn pre_seeded_target_never_consults_the_ownership_source()
    -> Result<(), Box<dyn std::error::Error>> {
        let own = WorkflowId::new(uuid::Uuid::from_u128(1));
        let counting = CountingOwnership {
            inner: StaticWorkflowNamespaces::default(),
            reads: std::sync::atomic::AtomicUsize::new(0),
            fail_after: 0,
        };
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            counting,
            StaticScheduleNamespaces::default(),
        );
        let mut gate = NamespaceEventGate::new(resolver, "tenant-a".to_owned());
        gate.allow(own.clone());

        assert!(gate.permits(&event(1, &own)?).await?);
        Ok(())
    }

    #[tokio::test]
    async fn ownership_read_failure_propagates_instead_of_guessing()
    -> Result<(), Box<dyn std::error::Error>> {
        let own = WorkflowId::new(uuid::Uuid::from_u128(1));
        let counting = CountingOwnership {
            inner: StaticWorkflowNamespaces::default(),
            reads: std::sync::atomic::AtomicUsize::new(0),
            fail_after: 0,
        };
        let resolver = NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            counting,
            StaticScheduleNamespaces::default(),
        );
        let mut gate = NamespaceEventGate::new(resolver, "tenant-a".to_owned());

        let error = gate.permits(&event(1, &own)?).await.err();
        assert!(matches!(error, Some(ServerError::Config { .. })));
        Ok(())
    }
}
