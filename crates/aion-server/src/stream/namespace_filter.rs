//! Namespace-aware event gating at the broadcast/encode seam.
//!
//! The engine's broadcast channel is engine-global and `EventFilter` has no
//! namespace dimension, while one shared `Engine` serves every tenant. Every
//! subscription kind — per-workflow, filtered, and firehose — must therefore
//! pass each live event through this gate before a frame is encoded, so a
//! tenant's socket can never receive (or be labeled with) another tenant's
//! events.

use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::sync::Arc;

use aion_core::{Event, WorkflowId};

use crate::error::ServerError;
use crate::namespace::NamespaceResolver;

/// Per-event admission decision returned by [`NamespaceEventGate::admit`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GateVerdict {
    /// The event's workflow is owned by the authorized namespace and may be
    /// delivered; carries the workflow's recorded type for selector matching.
    Permitted {
        /// Workflow type recorded for the owning workflow, when known.
        workflow_type: Option<Arc<str>>,
    },
    /// The event's workflow is foreign or unknown to the authorized namespace
    /// and must be silently filtered out (a firehose has no entitlement to
    /// learn that foreign events exist at all).
    Filtered,
}

/// Per-connection gate deciding whether an event belongs to the authorized
/// namespace, and what workflow type its workflow carries.
///
/// Verdicts are cached per workflow in a bounded LRU. The cache is sound
/// because a workflow's owner namespace is recorded atomically with its
/// `WorkflowStarted` batch and never changes, and the publisher broadcasts
/// only after durable commit — so by the time any event for a workflow is
/// observed here, its ownership verdict is durable and final, and an evicted
/// entry that is later re-read always reproduces the same verdict.
///
/// The cache bound is derived from the configured
/// `websocket.event_broadcast_capacity` rather than introducing a new knob:
/// the engine-global broadcast channel retains at most that many events, so
/// any burst this connection can observe without lagging out references at
/// most that many distinct workflows. Sizing the LRU to the broadcast
/// capacity keeps every workflow of the largest possible in-flight window
/// cached; anything beyond it is cold traffic where an eviction costs one
/// re-read of immutable durable state.
pub struct NamespaceEventGate {
    resolver: NamespaceResolver,
    namespace: String,
    verdicts: VerdictCache,
}

impl NamespaceEventGate {
    /// Build a gate for one authorized namespace with a bounded verdict cache.
    #[must_use]
    pub fn new(
        resolver: NamespaceResolver,
        namespace: String,
        verdict_capacity: NonZeroUsize,
    ) -> Self {
        Self {
            resolver,
            namespace,
            verdicts: VerdictCache::new(verdict_capacity),
        }
    }

    /// Pre-seed an allow verdict for a workflow whose ownership the namespace
    /// guard already verified (the per-workflow subscription target), so the
    /// hot path never re-reads history for it. The workflow type is captured
    /// lazily from the stream or a later attribution read; per-workflow
    /// subscriptions carry no type selector, so none is needed up front.
    pub fn allow(&mut self, workflow_id: WorkflowId) {
        self.verdicts.insert(
            workflow_id,
            CachedVerdict {
                permitted: true,
                workflow_type: None,
            },
        );
    }

    /// Decide whether `event` may be delivered to this connection.
    ///
    /// [`GateVerdict::Filtered`] means the event's workflow is foreign or
    /// unknown to the authorized namespace. [`GateVerdict::Permitted`] carries
    /// the workflow's recorded type so selector filtering can run on the same
    /// cached read that proved ownership.
    ///
    /// A delivered `WorkflowStarted` event refreshes the cached type inline
    /// (continue-as-new chains record each run's type on its own
    /// `WorkflowStarted`), so the cached type follows the stream's own order
    /// without extra reads.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when the durable ownership source cannot be
    /// read; callers must terminate the stream loudly rather than guessing.
    pub async fn admit(&mut self, event: &Event) -> Result<GateVerdict, ServerError> {
        let workflow_id = event.workflow_id();
        let cached = if let Some(verdict) = self.verdicts.get(workflow_id) {
            verdict
        } else {
            // One durable read answers both the namespace verdict and the
            // workflow type. Foreign-owned and unknown workflows are an
            // identical `None` (anti-existence-leak) and cache as denied.
            let verdict = match self
                .resolver
                .workflow_attribution(&self.namespace, workflow_id)
                .await?
            {
                Some(attribution) => CachedVerdict {
                    permitted: true,
                    workflow_type: attribution.workflow_type.map(Arc::from),
                },
                None => CachedVerdict {
                    permitted: false,
                    workflow_type: None,
                },
            };
            self.verdicts.insert(workflow_id.clone(), verdict.clone());
            verdict
        };
        if !cached.permitted {
            return Ok(GateVerdict::Filtered);
        }
        let workflow_type = if let Event::WorkflowStarted { workflow_type, .. } = event {
            let inline: Arc<str> = Arc::from(workflow_type.as_str());
            self.verdicts.refresh_type(workflow_id, Arc::clone(&inline));
            Some(inline)
        } else {
            cached.workflow_type
        };
        Ok(GateVerdict::Permitted { workflow_type })
    }
}

/// Cached per-workflow verdict: namespace admission plus recorded type.
#[derive(Clone, Debug)]
struct CachedVerdict {
    permitted: bool,
    workflow_type: Option<Arc<str>>,
}

/// Bounded least-recently-used verdict cache.
///
/// `entries` owns the verdicts keyed by workflow; `order` maps a strictly
/// increasing access stamp to the workflow it last touched, so the
/// least-recently-used entry is always `order`'s first key. Every operation is
/// `O(log n)`.
struct VerdictCache {
    capacity: NonZeroUsize,
    entries: HashMap<WorkflowId, StampedVerdict>,
    order: BTreeMap<u64, WorkflowId>,
    clock: u64,
}

struct StampedVerdict {
    stamp: u64,
    verdict: CachedVerdict,
}

impl VerdictCache {
    fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            order: BTreeMap::new(),
            clock: 0,
        }
    }

    fn next_stamp(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn get(&mut self, workflow_id: &WorkflowId) -> Option<CachedVerdict> {
        let stamp = self.next_stamp();
        let entry = self.entries.get_mut(workflow_id)?;
        self.order.remove(&entry.stamp);
        entry.stamp = stamp;
        self.order.insert(stamp, workflow_id.clone());
        Some(entry.verdict.clone())
    }

    fn insert(&mut self, workflow_id: WorkflowId, verdict: CachedVerdict) {
        let stamp = self.next_stamp();
        if let Some(existing) = self.entries.get_mut(&workflow_id) {
            self.order.remove(&existing.stamp);
            existing.stamp = stamp;
            existing.verdict = verdict;
            self.order.insert(stamp, workflow_id);
            return;
        }
        if self.entries.len() >= self.capacity.get() {
            if let Some((&oldest_stamp, _)) = self.order.first_key_value() {
                if let Some(evicted) = self.order.remove(&oldest_stamp) {
                    self.entries.remove(&evicted);
                }
            }
        }
        self.entries
            .insert(workflow_id.clone(), StampedVerdict { stamp, verdict });
        self.order.insert(stamp, workflow_id);
    }

    fn refresh_type(&mut self, workflow_id: &WorkflowId, workflow_type: Arc<str>) {
        if let Some(entry) = self.entries.get_mut(workflow_id) {
            entry.verdict.workflow_type = Some(workflow_type);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        debug_assert_eq!(self.entries.len(), self.order.len());
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use aion_core::{Event, EventEnvelope, Payload, WorkflowId};
    use async_trait::async_trait;

    use super::{GateVerdict, NamespaceEventGate};
    use crate::config::NamespaceMode;
    use crate::error::ServerError;
    use crate::namespace::{
        NamespaceResolver, StaticScheduleNamespaces, StaticWorkflowNamespaces, WorkflowAttribution,
        WorkflowNamespaceSource,
    };

    fn capacity(value: usize) -> Result<NonZeroUsize, Box<dyn std::error::Error>> {
        NonZeroUsize::new(value).ok_or_else(|| "capacity must be non-zero".into())
    }

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

    fn started(
        seq: u64,
        workflow_id: &WorkflowId,
        workflow_type: &str,
    ) -> Result<Event, aion_core::PayloadError> {
        Ok(Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq,
                recorded_at: chrono::Utc::now(),
                workflow_id: workflow_id.clone(),
            },
            workflow_type: workflow_type.to_owned(),
            input: Payload::from_json(&serde_json::json!({ "seq": seq }))?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(u128::from(seq))),
            parent_run_id: None,
        })
    }

    fn resolver(ownership: StaticWorkflowNamespaces) -> NamespaceResolver {
        NamespaceResolver::authorization_only(
            NamespaceMode::SharedEngine,
            ownership,
            StaticScheduleNamespaces::default(),
        )
    }

    fn permitted(verdict: &GateVerdict) -> bool {
        matches!(verdict, GateVerdict::Permitted { .. })
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
        let mut gate =
            NamespaceEventGate::new(resolver(ownership), "tenant-a".to_owned(), capacity(8)?);

        assert!(permitted(&gate.admit(&event(1, &own)?).await?));
        assert_eq!(
            gate.admit(&event(1, &foreign)?).await?,
            GateVerdict::Filtered
        );
        assert_eq!(
            gate.admit(&event(1, &unknown)?).await?,
            GateVerdict::Filtered
        );
        Ok(())
    }

    #[tokio::test]
    async fn admit_carries_the_recorded_workflow_type() -> Result<(), Box<dyn std::error::Error>> {
        let own = WorkflowId::new(uuid::Uuid::from_u128(1));
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record_with_type(own.clone(), "tenant-a", "checkout")?;
        let mut gate =
            NamespaceEventGate::new(resolver(ownership), "tenant-a".to_owned(), capacity(8)?);

        // A non-started event first-sighted mid-stream still learns the type
        // from the same durable read that proved ownership.
        let verdict = gate.admit(&event(5, &own)?).await?;
        let GateVerdict::Permitted { workflow_type } = verdict else {
            return Err("owned workflow must be permitted".into());
        };
        assert_eq!(workflow_type.as_deref(), Some("checkout"));
        Ok(())
    }

    #[tokio::test]
    async fn workflow_started_refreshes_the_cached_type_inline()
    -> Result<(), Box<dyn std::error::Error>> {
        let own = WorkflowId::new(uuid::Uuid::from_u128(1));
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record_with_type(own.clone(), "tenant-a", "checkout")?;
        let mut gate =
            NamespaceEventGate::new(resolver(ownership), "tenant-a".to_owned(), capacity(8)?);

        let first = gate.admit(&event(1, &own)?).await?;
        let GateVerdict::Permitted { workflow_type } = first else {
            return Err("owned workflow must be permitted".into());
        };
        assert_eq!(workflow_type.as_deref(), Some("checkout"));

        // Continue-as-new boundary: the new run's WorkflowStarted carries the
        // migrated type and must refresh the cache for subsequent events.
        let started_verdict = gate.admit(&started(2, &own, "checkout-v2")?).await?;
        let GateVerdict::Permitted { workflow_type } = started_verdict else {
            return Err("owned workflow must be permitted".into());
        };
        assert_eq!(workflow_type.as_deref(), Some("checkout-v2"));

        let after = gate.admit(&event(3, &own)?).await?;
        let GateVerdict::Permitted { workflow_type } = after else {
            return Err("owned workflow must be permitted".into());
        };
        assert_eq!(workflow_type.as_deref(), Some("checkout-v2"));
        Ok(())
    }

    /// Ownership source that counts reads and fails after a configurable
    /// number so the cache and the loud-failure path can both be proven.
    struct CountingOwnership {
        inner: StaticWorkflowNamespaces,
        reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        fail_after: usize,
    }

    #[async_trait]
    impl WorkflowNamespaceSource for CountingOwnership {
        async fn workflow_attribution(
            &self,
            workflow_id: &WorkflowId,
        ) -> Result<Option<WorkflowAttribution>, ServerError> {
            let reads = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if reads >= self.fail_after {
                return Err(ServerError::Config {
                    message: "ownership source unavailable".to_owned(),
                });
            }
            self.inner.workflow_attribution(workflow_id).await
        }
    }

    fn counting_resolver(
        inner: StaticWorkflowNamespaces,
        fail_after: usize,
    ) -> (
        NamespaceResolver,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counting = CountingOwnership {
            inner,
            reads: std::sync::Arc::clone(&reads),
            fail_after,
        };
        (
            NamespaceResolver::authorization_only(
                NamespaceMode::SharedEngine,
                counting,
                StaticScheduleNamespaces::default(),
            ),
            reads,
        )
    }

    #[tokio::test]
    async fn verdicts_are_cached_per_workflow() -> Result<(), Box<dyn std::error::Error>> {
        let own = WorkflowId::new(uuid::Uuid::from_u128(1));
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(own.clone(), "tenant-a")?;
        let (resolver, _reads) = counting_resolver(ownership, 1);
        let mut gate = NamespaceEventGate::new(resolver, "tenant-a".to_owned(), capacity(8)?);

        // Second admit() must hit the cache; a second source read would fail.
        assert!(permitted(&gate.admit(&event(1, &own)?).await?));
        assert!(permitted(&gate.admit(&event(2, &own)?).await?));
        Ok(())
    }

    /// FINDING L1: the verdict cache is bounded. A hostile/busy shared engine
    /// streaming events from unbounded distinct workflows must never grow the
    /// per-connection cache past its LRU bound; evicted entries are re-read on
    /// the next sighting and reproduce the same verdict (ownership is
    /// immutable, so eviction plus re-read is always consistent).
    #[tokio::test]
    async fn verdict_cache_is_bounded_and_eviction_rereads_consistently()
    -> Result<(), Box<dyn std::error::Error>> {
        let first = WorkflowId::new(uuid::Uuid::from_u128(1));
        let second = WorkflowId::new(uuid::Uuid::from_u128(2));
        let third = WorkflowId::new(uuid::Uuid::from_u128(3));
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(first.clone(), "tenant-a")?;
        ownership.record(second.clone(), "tenant-b")?;
        ownership.record(third.clone(), "tenant-a")?;
        let (resolver, reads) = counting_resolver(ownership, usize::MAX);
        let mut gate = NamespaceEventGate::new(resolver, "tenant-a".to_owned(), capacity(2)?);

        assert!(permitted(&gate.admit(&event(1, &first)?).await?));
        assert_eq!(
            gate.admit(&event(1, &second)?).await?,
            GateVerdict::Filtered
        );
        // Third distinct workflow evicts `first` (the least recently used).
        assert!(permitted(&gate.admit(&event(1, &third)?).await?));
        assert_eq!(gate.verdicts.len(), 2, "cache must never exceed its bound");
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 3);

        // Re-sighting the evicted workflow costs exactly one re-read and
        // reproduces the identical verdict.
        assert!(permitted(&gate.admit(&event(2, &first)?).await?));
        assert_eq!(gate.verdicts.len(), 2, "cache must never exceed its bound");
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 4);
        Ok(())
    }

    /// Recency, not insertion order, must drive eviction: touching the oldest
    /// entry protects it and the untouched middle entry is evicted instead.
    #[tokio::test]
    async fn lru_eviction_respects_recency() -> Result<(), Box<dyn std::error::Error>> {
        let first = WorkflowId::new(uuid::Uuid::from_u128(1));
        let second = WorkflowId::new(uuid::Uuid::from_u128(2));
        let third = WorkflowId::new(uuid::Uuid::from_u128(3));
        let ownership = StaticWorkflowNamespaces::default();
        ownership.record(first.clone(), "tenant-a")?;
        ownership.record(second.clone(), "tenant-a")?;
        ownership.record(third.clone(), "tenant-a")?;
        let (resolver, reads) = counting_resolver(ownership, usize::MAX);
        let mut gate = NamespaceEventGate::new(resolver, "tenant-a".to_owned(), capacity(2)?);

        assert!(permitted(&gate.admit(&event(1, &first)?).await?));
        assert!(permitted(&gate.admit(&event(1, &second)?).await?));
        // Touch `first` so `second` becomes least recently used.
        assert!(permitted(&gate.admit(&event(2, &first)?).await?));
        assert!(permitted(&gate.admit(&event(1, &third)?).await?));
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 3);

        // `first` must still be cached (no new read)…
        assert!(permitted(&gate.admit(&event(3, &first)?).await?));
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 3);
        // …while `second` was evicted and costs a re-read.
        assert!(permitted(&gate.admit(&event(2, &second)?).await?));
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 4);
        Ok(())
    }

    #[tokio::test]
    async fn pre_seeded_target_never_consults_the_ownership_source()
    -> Result<(), Box<dyn std::error::Error>> {
        let own = WorkflowId::new(uuid::Uuid::from_u128(1));
        let (resolver, _reads) = counting_resolver(StaticWorkflowNamespaces::default(), 0);
        let mut gate = NamespaceEventGate::new(resolver, "tenant-a".to_owned(), capacity(8)?);
        gate.allow(own.clone());

        assert!(permitted(&gate.admit(&event(1, &own)?).await?));
        Ok(())
    }

    #[tokio::test]
    async fn ownership_read_failure_propagates_instead_of_guessing()
    -> Result<(), Box<dyn std::error::Error>> {
        let own = WorkflowId::new(uuid::Uuid::from_u128(1));
        let (resolver, _reads) = counting_resolver(StaticWorkflowNamespaces::default(), 0);
        let mut gate = NamespaceEventGate::new(resolver, "tenant-a".to_owned(), capacity(8)?);

        let error = gate.admit(&event(1, &own)?).await.err();
        assert!(matches!(error, Some(ServerError::Config { .. })));
        Ok(())
    }
}
