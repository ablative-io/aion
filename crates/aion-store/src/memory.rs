//! `InMemoryStore` reference implementation and behavioural test suite.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};

use aion_core::{
    Event, TimerId, WorkflowFilter, WorkflowId, WorkflowStatus, WorkflowSummary, status_from_events,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::namespace::{
    MintOutcome, NamespaceOrigin, NamespacePlacement, NamespaceRecord, NamespaceState,
    NamespaceStore,
};
use crate::package::{PackageRecord, PackageRouteRecord, PackageStore};
use crate::visibility::{ListWorkflowsFilter, VisibilityRecord, VisibilityStore};
use crate::{
    ReadableEventStore, RunSummary, StoreError, TimerEntry, WritableEventStore, WriteToken,
};

/// Correct non-durable [`crate::EventStore`] implementation for tests and backend equivalence.
#[derive(Debug, Default)]
pub struct InMemoryStore {
    state: Mutex<InMemoryState>,
    namespaces: Mutex<BTreeMap<String, NamespaceRecord>>,
}

#[async_trait]
impl VisibilityStore for InMemoryStore {
    async fn record_visibility(&self, record: VisibilityRecord) -> Result<(), StoreError> {
        let mut state = self.lock_state()?;
        state
            .visibility
            .insert((record.workflow_id.clone(), record.run_id.clone()), record);
        Ok(())
    }

    async fn list_workflows(
        &self,
        filter: ListWorkflowsFilter,
    ) -> Result<Vec<crate::visibility::WorkflowSummary>, StoreError> {
        let state = self.lock_state()?;
        let mut summaries = state
            .visibility
            .values()
            .cloned()
            .map(crate::visibility::WorkflowSummary::from)
            .filter(|summary| filter.matches(summary))
            .collect::<Vec<_>>();
        summaries.sort_by(|left, right| {
            left.start_time.cmp(&right.start_time).then_with(|| {
                left.workflow_id
                    .to_string()
                    .cmp(&right.workflow_id.to_string())
            })
        });
        let offset = filter.offset.and_then(|value| usize::try_from(value).ok());
        if let Some(offset) = offset {
            summaries = summaries.into_iter().skip(offset).collect();
        }
        if let Some(limit) = filter.limit.and_then(|value| usize::try_from(value).ok()) {
            summaries.truncate(limit);
        }
        Ok(summaries)
    }

    async fn count_workflows(&self, filter: ListWorkflowsFilter) -> Result<u64, StoreError> {
        let state = self.lock_state()?;
        Ok(state
            .visibility
            .values()
            .cloned()
            .map(crate::visibility::WorkflowSummary::from)
            .filter(|summary| filter.matches(summary))
            .count()
            .try_into()
            .unwrap_or(u64::MAX))
    }
}

#[derive(Debug, Default)]
struct InMemoryState {
    histories: HashMap<WorkflowId, Vec<Event>>,
    timers: HashMap<(WorkflowId, TimerId), TimerEntry>,
    visibility: HashMap<(WorkflowId, aion_core::RunId), VisibilityRecord>,
    packages: HashMap<(String, String), PackageRecord>,
    package_routes: HashMap<String, String>,
}

impl InMemoryStore {
    fn lock_state(&self) -> Result<MutexGuard<'_, InMemoryState>, StoreError> {
        self.state
            .lock()
            .map_err(|error| StoreError::Backend(format!("in-memory store lock poisoned: {error}")))
    }

    /// Poison-tolerant lock over the namespace registry map.
    ///
    /// The namespace operations have no fallible body beyond the lock, so a
    /// poisoned guard is recovered in place (matching the recording test-double
    /// pattern in `testing.rs`) rather than surfaced as a `StoreError`.
    fn lock_namespaces(&self) -> MutexGuard<'_, BTreeMap<String, NamespaceRecord>> {
        self.namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

fn history_head(history: &[Event]) -> u64 {
    history.iter().map(Event::seq).max().unwrap_or_default()
}

fn history_in_sequence_order(history: &[Event]) -> Vec<Event> {
    let mut ordered = history.to_vec();
    ordered.sort_by_key(Event::seq);
    ordered
}

#[async_trait]
impl PackageStore for InMemoryStore {
    async fn put_package(&self, record: PackageRecord) -> Result<(), StoreError> {
        let primary = record.workflow_type.clone();
        self.put_package_with_routes(record, &[primary]).await
    }

    async fn put_package_with_routes(
        &self,
        record: PackageRecord,
        route_workflow_types: &[String],
    ) -> Result<(), StoreError> {
        let mut state = self.lock_state()?;
        for workflow_type in route_workflow_types {
            state
                .package_routes
                .insert(workflow_type.clone(), record.content_hash.clone());
        }
        state.packages.insert(
            (record.workflow_type.clone(), record.content_hash.clone()),
            record,
        );
        Ok(())
    }

    async fn list_packages(&self) -> Result<Vec<PackageRecord>, StoreError> {
        let state = self.lock_state()?;
        let mut records: Vec<PackageRecord> = state.packages.values().cloned().collect();
        records.sort_by(|left, right| {
            left.deployed_at
                .cmp(&right.deployed_at)
                .then_with(|| left.workflow_type.cmp(&right.workflow_type))
                .then_with(|| left.content_hash.cmp(&right.content_hash))
        });
        Ok(records)
    }

    async fn delete_package(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        let mut state = self.lock_state()?;
        state
            .packages
            .remove(&(workflow_type.to_owned(), content_hash.to_owned()));
        Ok(())
    }

    async fn put_package_route(
        &self,
        workflow_type: &str,
        content_hash: &str,
    ) -> Result<(), StoreError> {
        let mut state = self.lock_state()?;
        state
            .package_routes
            .insert(workflow_type.to_owned(), content_hash.to_owned());
        Ok(())
    }

    async fn list_package_routes(&self) -> Result<Vec<PackageRouteRecord>, StoreError> {
        let state = self.lock_state()?;
        let mut routes: Vec<PackageRouteRecord> = state
            .package_routes
            .iter()
            .map(|(workflow_type, content_hash)| PackageRouteRecord {
                workflow_type: workflow_type.clone(),
                content_hash: content_hash.clone(),
            })
            .collect();
        routes.sort_by(|left, right| left.workflow_type.cmp(&right.workflow_type));
        Ok(routes)
    }
}

#[async_trait]
impl NamespaceStore for InMemoryStore {
    async fn register_namespace(
        &self,
        name: &str,
        origin: NamespaceOrigin,
    ) -> Result<MintOutcome, StoreError> {
        let now = Utc::now();
        let mut namespaces = self.lock_namespaces();
        if let Some(existing) = namespaces.get_mut(name) {
            existing.bump_last_seen(now);
            Ok(MintOutcome::AlreadyExisted)
        } else {
            namespaces.insert(
                name.to_owned(),
                NamespaceRecord::new_minted(name, origin, now),
            );
            Ok(MintOutcome::Created)
        }
    }

    async fn put_namespace(&self, record: NamespaceRecord) -> Result<MintOutcome, StoreError> {
        let now = Utc::now();
        let mut namespaces = self.lock_namespaces();
        if let Some(existing) = namespaces.get_mut(&record.name) {
            // Idempotent on an existing name: reconcile as already-existing
            // rather than overwriting the durable record wholesale. Only the
            // staleness signal is refreshed.
            existing.bump_last_seen(now);
            Ok(MintOutcome::AlreadyExisted)
        } else {
            namespaces.insert(record.name.clone(), record);
            Ok(MintOutcome::Created)
        }
    }

    async fn list_namespaces(&self) -> Result<Vec<NamespaceRecord>, StoreError> {
        let namespaces = self.lock_namespaces();
        let mut records: Vec<NamespaceRecord> = namespaces.values().cloned().collect();
        records.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.name.cmp(&right.name))
        });
        Ok(records)
    }

    async fn get_namespace(&self, name: &str) -> Result<Option<NamespaceRecord>, StoreError> {
        let namespaces = self.lock_namespaces();
        Ok(namespaces.get(name).cloned())
    }

    async fn set_namespace_placement(
        &self,
        name: &str,
        placement: NamespacePlacement,
    ) -> Result<Option<()>, StoreError> {
        let now = Utc::now();
        let mut namespaces = self.lock_namespaces();
        let Some(existing) = namespaces.get_mut(name) else {
            // Placement targets an already-minted namespace: an absent row is a
            // not-found the caller surfaces, never a silent mint here.
            return Ok(None);
        };
        existing.placement = placement;
        existing.bump_last_seen(now);
        Ok(Some(()))
    }

    async fn deprecate_namespace(&self, name: &str) -> Result<(), StoreError> {
        let mut namespaces = self.lock_namespaces();
        if let Some(existing) = namespaces.get_mut(name) {
            existing.state = NamespaceState::Deprecated;
        }
        // A missing row is an idempotent no-op: deprecation never strands
        // durable history and an absent registry entry is not an error.
        Ok(())
    }
}

#[async_trait]
impl WritableEventStore for InMemoryStore {
    async fn append(
        &self,
        _token: WriteToken,
        workflow_id: &WorkflowId,
        events: &[Event],
        expected_seq: u64,
    ) -> Result<(), StoreError> {
        let mut state = self.lock_state()?;
        let current_head = state
            .histories
            .get(workflow_id)
            .map_or(0, |history| history_head(history));

        if current_head != expected_seq {
            return Err(StoreError::SequenceConflict {
                expected: expected_seq,
                found: current_head,
            });
        }

        if events.is_empty() {
            return Ok(());
        }

        for (next_seq, event) in (expected_seq + 1..).zip(events.iter()) {
            if event.seq() != next_seq {
                return Err(StoreError::Backend(format!(
                    "event sequence must be contiguous: expected {next_seq}, got {}",
                    event.seq()
                )));
            }
        }

        state
            .histories
            .entry(workflow_id.clone())
            .or_default()
            .extend(events.iter().cloned());
        Ok(())
    }
}

#[async_trait]
impl ReadableEventStore for InMemoryStore {
    async fn read_history(&self, workflow_id: &WorkflowId) -> Result<Vec<Event>, StoreError> {
        let state = self.lock_state()?;
        Ok(state
            .histories
            .get(workflow_id)
            .map_or_else(Vec::new, |history| history_in_sequence_order(history)))
    }

    async fn read_history_from(
        &self,
        workflow_id: &WorkflowId,
        from_seq: u64,
    ) -> Result<Vec<Event>, StoreError> {
        let state = self.lock_state()?;
        Ok(state
            .histories
            .get(workflow_id)
            .map_or_else(Vec::new, |history| {
                let mut events = history
                    .iter()
                    .filter(|event| event.seq() >= from_seq)
                    .cloned()
                    .collect::<Vec<_>>();
                events.sort_by_key(Event::seq);
                events
            }))
    }

    async fn read_run_chain(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<Vec<RunSummary>, StoreError> {
        let state = self.lock_state()?;
        let Some(history) = state.histories.get(workflow_id) else {
            return Ok(Vec::new());
        };

        crate::run_chain::run_chain_from_history(history)
    }

    async fn list_workflow_ids(&self) -> Result<Vec<WorkflowId>, StoreError> {
        let state = self.lock_state()?;
        let mut workflow_ids = state.histories.keys().cloned().collect::<Vec<_>>();
        workflow_ids.sort_by_key(ToString::to_string);
        Ok(workflow_ids)
    }

    async fn list_active(&self) -> Result<Vec<WorkflowId>, StoreError> {
        let state = self.lock_state()?;
        let mut active = state
            .histories
            .iter()
            .filter(|(_, history)| {
                matches!(
                    status_from_events(&history_in_sequence_order(history)),
                    WorkflowStatus::Running
                )
            })
            .map(|(workflow_id, _)| workflow_id.clone())
            .collect::<Vec<_>>();
        active.sort_by_key(ToString::to_string);
        Ok(active)
    }

    async fn list_paused(&self) -> Result<Vec<WorkflowId>, StoreError> {
        let state = self.lock_state()?;
        let mut paused = state
            .histories
            .iter()
            .filter(|(_, history)| {
                matches!(
                    status_from_events(&history_in_sequence_order(history)),
                    WorkflowStatus::Paused
                )
            })
            .map(|(workflow_id, _)| workflow_id.clone())
            .collect::<Vec<_>>();
        paused.sort_by_key(ToString::to_string);
        Ok(paused)
    }

    async fn query(&self, filter: &WorkflowFilter) -> Result<Vec<WorkflowSummary>, StoreError> {
        let state = self.lock_state()?;
        let mut summaries = state
            .histories
            .values()
            .filter_map(|history| {
                WorkflowSummary::from_history(&history_in_sequence_order(history))
            })
            .filter(|summary| filter.matches(summary))
            .collect::<Vec<_>>();
        summaries.sort_by(|left, right| {
            left.started_at.cmp(&right.started_at).then_with(|| {
                left.workflow_id
                    .to_string()
                    .cmp(&right.workflow_id.to_string())
            })
        });
        Ok(summaries)
    }

    async fn schedule_timer(
        &self,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
        fire_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut state = self.lock_state()?;
        state.timers.insert(
            (workflow_id.clone(), timer_id.clone()),
            TimerEntry {
                workflow_id: workflow_id.clone(),
                timer_id: timer_id.clone(),
                fire_at,
            },
        );
        Ok(())
    }

    async fn expired_timers(&self, as_of: DateTime<Utc>) -> Result<Vec<TimerEntry>, StoreError> {
        let state = self.lock_state()?;
        let mut timers = state
            .timers
            .values()
            .filter(|entry| entry.fire_at <= as_of)
            .cloned()
            .collect::<Vec<_>>();
        timers.sort_by(|left, right| {
            left.fire_at
                .cmp(&right.fire_at)
                .then_with(|| {
                    left.workflow_id
                        .to_string()
                        .cmp(&right.workflow_id.to_string())
                })
                .then_with(|| left.timer_id.to_string().cmp(&right.timer_id.to_string()))
        });
        Ok(timers)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{
        Event, EventEnvelope, Payload, TimerId, WorkflowError, WorkflowFilter, WorkflowId,
        WorkflowStatus,
    };
    use chrono::{DateTime, Utc};
    use serde_json::json;
    use tokio::task;
    use uuid::Uuid;

    use super::InMemoryStore;
    use crate::{ReadableEventStore, StoreError, TimerEntry, WritableEventStore, WriteToken};

    fn write_token() -> WriteToken {
        WriteToken::recorder()
    }

    fn recorded_at(offset_seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + offset_seconds, 0).unwrap_or_default()
    }

    fn workflow_id(value: u128) -> WorkflowId {
        WorkflowId::new(Uuid::from_u128(value))
    }

    fn envelope(seq: u64, workflow_id: &WorkflowId) -> EventEnvelope {
        EventEnvelope {
            seq,
            recorded_at: recorded_at(i64::try_from(seq).unwrap_or_default()),
            workflow_id: workflow_id.clone(),
        }
    }

    fn run_id(value: u128) -> aion_core::RunId {
        aion_core::RunId::new(Uuid::from_u128(value))
    }

    fn payload(label: &str) -> Payload {
        Payload::from_json(&json!({ "label": label })).unwrap_or_else(|error| {
            Payload::new(
                aion_core::ContentType::Json,
                format!("{{\"payload_error\":\"{error}\"}}").into_bytes(),
            )
        })
    }

    fn workflow_started(seq: u64, workflow_id: &WorkflowId, workflow_type: &str) -> Event {
        Event::WorkflowStarted {
            envelope: envelope(seq, workflow_id),
            workflow_type: workflow_type.to_owned(),
            input: payload("input"),
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        }
    }

    fn workflow_completed(seq: u64, workflow_id: &WorkflowId) -> Event {
        Event::WorkflowCompleted {
            envelope: envelope(seq, workflow_id),
            result: payload("result"),
        }
    }

    fn workflow_failed(seq: u64, workflow_id: &WorkflowId) -> Event {
        Event::WorkflowFailed {
            envelope: envelope(seq, workflow_id),
            error: WorkflowError {
                message: String::from("failed"),
                details: None,
            },
        }
    }

    #[tokio::test]
    async fn read_history_returns_empty_for_unknown_workflow() -> Result<(), StoreError> {
        let store = InMemoryStore::default();

        assert_eq!(store.read_history(&workflow_id(1)).await?, Vec::new());
        Ok(())
    }

    #[tokio::test]
    async fn append_preserves_sequence_order() -> Result<(), StoreError> {
        let store = InMemoryStore::default();
        let workflow_id = workflow_id(1);
        let first = workflow_started(1, &workflow_id, "checkout");
        let second = workflow_completed(2, &workflow_id);

        store
            .append(write_token(), &workflow_id, std::slice::from_ref(&first), 0)
            .await?;
        store
            .append(
                write_token(),
                &workflow_id,
                std::slice::from_ref(&second),
                1,
            )
            .await?;

        assert_eq!(store.read_history(&workflow_id).await?, vec![first, second]);
        Ok(())
    }

    #[tokio::test]
    async fn list_active_returns_only_running_workflows() -> Result<(), StoreError> {
        let store = InMemoryStore::default();
        let running = workflow_id(1);
        let completed = workflow_id(2);

        store
            .append(
                write_token(),
                &running,
                &[workflow_started(1, &running, "checkout")],
                0,
            )
            .await?;
        store
            .append(
                write_token(),
                &completed,
                &[
                    workflow_started(1, &completed, "checkout"),
                    workflow_completed(2, &completed),
                ],
                0,
            )
            .await?;

        assert_eq!(store.list_active().await?, vec![running]);
        Ok(())
    }

    fn workflow_paused(seq: u64, workflow_id: &WorkflowId) -> Event {
        Event::WorkflowPaused {
            envelope: envelope(seq, workflow_id),
            run_id: run_id(1),
            reason: None,
            operator: None,
        }
    }

    /// #204 GATE-2 recovery mechanism: a paused run is EXCLUDED from `list_active`
    /// (== Running) — so it is not respawned on restart — and is the sole content
    /// of `list_paused` (== Paused), the durable source the dispatch-hold is
    /// rebuilt from.
    #[tokio::test]
    async fn list_paused_and_list_active_partition_by_projected_status() -> Result<(), StoreError> {
        let store = InMemoryStore::default();
        let running = workflow_id(1);
        let paused = workflow_id(2);

        store
            .append(
                write_token(),
                &running,
                &[workflow_started(1, &running, "checkout")],
                0,
            )
            .await?;
        store
            .append(
                write_token(),
                &paused,
                &[
                    workflow_started(1, &paused, "checkout"),
                    workflow_paused(2, &paused),
                ],
                0,
            )
            .await?;

        assert_eq!(
            store.list_active().await?,
            vec![running],
            "a paused run is excluded from list_active (not respawned)"
        );
        assert_eq!(
            store.list_paused().await?,
            vec![paused],
            "list_paused returns exactly the paused run (the hold rebuild source)"
        );
        Ok(())
    }

    #[tokio::test]
    async fn list_workflow_ids_returns_running_and_terminal_histories() -> Result<(), StoreError> {
        let store = InMemoryStore::default();
        let running = workflow_id(2);
        let completed = workflow_id(1);

        store
            .append(
                write_token(),
                &running,
                &[workflow_started(1, &running, "checkout")],
                0,
            )
            .await?;
        store
            .append(
                write_token(),
                &completed,
                &[
                    workflow_started(1, &completed, "checkout"),
                    workflow_completed(2, &completed),
                ],
                0,
            )
            .await?;

        assert_eq!(store.list_workflow_ids().await?, vec![completed, running]);
        Ok(())
    }

    #[tokio::test]
    async fn read_run_chain_projects_run_id_from_started_event() -> Result<(), StoreError> {
        let store = InMemoryStore::default();
        let workflow_id = workflow_id(1);

        store
            .append(
                write_token(),
                &workflow_id,
                &[
                    workflow_started(1, &workflow_id, "checkout"),
                    workflow_completed(2, &workflow_id),
                ],
                0,
            )
            .await?;

        let chain = store.read_run_chain(&workflow_id).await?;

        assert_eq!(chain.len(), 1);
        // run_id comes from the WorkflowStarted event (hardcoded to from_u128(1) in the helper)
        assert_eq!(chain[0].run_id, run_id(1));
        assert_eq!(chain[0].status, WorkflowStatus::Completed);
        assert_eq!(chain[0].closed_at, Some(recorded_at(2)));
        Ok(())
    }

    #[tokio::test]
    async fn query_uses_core_filter_semantics() -> Result<(), StoreError> {
        let store = InMemoryStore::default();
        let running_checkout = workflow_id(1);
        let completed_checkout = workflow_id(2);
        let failed_billing = workflow_id(3);

        store
            .append(
                write_token(),
                &running_checkout,
                &[workflow_started(1, &running_checkout, "checkout")],
                0,
            )
            .await?;
        store
            .append(
                write_token(),
                &completed_checkout,
                &[
                    workflow_started(1, &completed_checkout, "checkout"),
                    workflow_completed(2, &completed_checkout),
                ],
                0,
            )
            .await?;
        store
            .append(
                write_token(),
                &failed_billing,
                &[
                    workflow_started(1, &failed_billing, "billing"),
                    workflow_failed(2, &failed_billing),
                ],
                0,
            )
            .await?;

        let filter = WorkflowFilter {
            workflow_type: Some(String::from("checkout")),
            status: Some(WorkflowStatus::Completed),
            started_after: Some(recorded_at(1)),
            started_before: Some(recorded_at(1)),
            parent: None,
        };
        let summaries = store.query(&filter).await?;

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].workflow_id, completed_checkout);
        assert_eq!(summaries[0].status, WorkflowStatus::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn stale_expected_sequence_writes_nothing() -> Result<(), StoreError> {
        let store = InMemoryStore::default();
        let workflow_id = workflow_id(1);
        let first = workflow_started(1, &workflow_id, "checkout");

        store
            .append(write_token(), &workflow_id, std::slice::from_ref(&first), 0)
            .await?;
        let conflict = store
            .append(
                write_token(),
                &workflow_id,
                &[workflow_completed(2, &workflow_id)],
                0,
            )
            .await;

        assert_eq!(
            conflict,
            Err(StoreError::SequenceConflict {
                expected: 0,
                found: 1,
            })
        );
        assert_eq!(store.read_history(&workflow_id).await?, vec![first]);
        Ok(())
    }

    #[tokio::test]
    async fn append_rejects_non_contiguous_event_sequences() -> Result<(), StoreError> {
        let store = InMemoryStore::default();
        let wf = workflow_id(1);

        let result = store
            .append(
                write_token(),
                &wf,
                &[
                    workflow_started(1, &wf, "checkout"),
                    workflow_completed(5, &wf),
                ],
                0,
            )
            .await;

        assert!(result.is_err());
        assert!(matches!(result, Err(StoreError::Backend(_))));
        assert_eq!(store.read_history(&wf).await?, Vec::new());
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_appends_on_same_expected_sequence_conflict_once() -> Result<(), StoreError>
    {
        let store = Arc::new(InMemoryStore::default());
        let workflow_id = workflow_id(1);
        let first_store = Arc::clone(&store);
        let first_workflow = workflow_id.clone();
        let second_store = Arc::clone(&store);
        let second_workflow = workflow_id.clone();

        let first = task::spawn(async move {
            first_store
                .append(
                    write_token(),
                    &first_workflow,
                    &[workflow_started(1, &first_workflow, "checkout")],
                    0,
                )
                .await
        });
        let second = task::spawn(async move {
            second_store
                .append(
                    write_token(),
                    &second_workflow,
                    &[workflow_completed(1, &second_workflow)],
                    0,
                )
                .await
        });

        let results = [
            first
                .await
                .map_err(|error| StoreError::Backend(format!("append task failed: {error}")))?,
            second
                .await
                .map_err(|error| StoreError::Backend(format!("append task failed: {error}")))?,
        ];

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(StoreError::SequenceConflict {
                        expected: 0,
                        found: 1
                    })
                ))
                .count(),
            1
        );
        assert_eq!(store.read_history(&workflow_id).await?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn rescheduling_same_timer_replaces_prior_fire_at() -> Result<(), StoreError> {
        let store = InMemoryStore::default();
        let workflow_id = workflow_id(1);
        let timer_id = TimerId::anonymous(1);
        let first_fire_at = recorded_at(10);
        let replacement_fire_at = recorded_at(30);

        store
            .schedule_timer(&workflow_id, &timer_id, first_fire_at)
            .await?;
        store
            .schedule_timer(&workflow_id, &timer_id, replacement_fire_at)
            .await?;

        assert_eq!(store.expired_timers(first_fire_at).await?, Vec::new());
        assert_eq!(
            store.expired_timers(replacement_fire_at).await?,
            vec![TimerEntry {
                workflow_id,
                timer_id,
                fire_at: replacement_fire_at,
            }]
        );
        Ok(())
    }

    #[tokio::test]
    async fn expired_timers_include_boundary_and_exclude_future() -> Result<(), StoreError> {
        let store = InMemoryStore::default();
        let workflow_id = workflow_id(1);
        let past_timer = TimerId::anonymous(1);
        let boundary_timer = TimerId::anonymous(2);
        let future_timer = TimerId::anonymous(3);
        let as_of = recorded_at(20);

        store
            .schedule_timer(&workflow_id, &future_timer, recorded_at(30))
            .await?;
        store
            .schedule_timer(&workflow_id, &boundary_timer, as_of)
            .await?;
        store
            .schedule_timer(&workflow_id, &past_timer, recorded_at(10))
            .await?;

        assert_eq!(
            store.expired_timers(as_of).await?,
            vec![
                TimerEntry {
                    workflow_id: workflow_id.clone(),
                    timer_id: past_timer,
                    fire_at: recorded_at(10),
                },
                TimerEntry {
                    workflow_id,
                    timer_id: boundary_timer,
                    fire_at: as_of,
                },
            ]
        );
        Ok(())
    }
}

#[cfg(test)]
mod namespace_tests {
    #![allow(clippy::expect_used)]

    use super::InMemoryStore;
    use crate::namespace::{
        MintOutcome, NamespaceOrigin, NamespacePlacement, NamespaceRecord, NamespaceState,
    };
    use crate::{NamespaceStore, StoreError};
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeSet;

    fn labels(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    /// `set_namespace_placement` updates ONLY the placement (+ `last_seen`) of an
    /// existing record, leaves the other fields untouched, is idempotent, and is a
    /// not-found (`Ok(None)`) for an absent namespace — never a silent mint.
    #[tokio::test]
    async fn set_placement_updates_only_placement_and_reports_not_found() -> Result<(), StoreError>
    {
        let store = InMemoryStore::default();
        store
            .register_namespace("orders", NamespaceOrigin::Explicit)
            .await?;
        let original = store
            .get_namespace("orders")
            .await?
            .expect("namespace must persist");
        assert_eq!(original.placement, NamespacePlacement::Unplaced);

        let placement = NamespacePlacement::Prefer {
            nodes: labels(&["n1", "n2"]),
        };
        assert_eq!(
            store
                .set_namespace_placement("orders", placement.clone())
                .await?,
            Some(())
        );
        let updated = store
            .get_namespace("orders")
            .await?
            .expect("namespace must persist");
        assert_eq!(updated.placement, placement);
        // Only placement + last_seen changed; identity/lifecycle preserved.
        assert_eq!(updated.origin, original.origin);
        assert_eq!(updated.created_at, original.created_at);
        assert_eq!(updated.state, original.state);

        // Idempotent: re-applying the same placement is a successful no-op.
        assert_eq!(
            store.set_namespace_placement("orders", placement).await?,
            Some(())
        );

        // Absent namespace: not-found, and nothing minted.
        assert_eq!(
            store
                .set_namespace_placement("ghost", NamespacePlacement::Unplaced)
                .await?,
            None
        );
        assert!(store.get_namespace("ghost").await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn register_creates_if_absent_and_persists() -> Result<(), StoreError> {
        let store = InMemoryStore::default();

        let outcome = store
            .register_namespace("orders", NamespaceOrigin::WorkerMint)
            .await?;

        assert_eq!(outcome, MintOutcome::Created);
        let record = store
            .get_namespace("orders")
            .await?
            .expect("namespace must persist");
        assert_eq!(record.name, "orders");
        assert_eq!(record.origin, NamespaceOrigin::WorkerMint);
        assert_eq!(record.state, NamespaceState::Active);
        assert_eq!(record.created_at, record.last_seen);
        Ok(())
    }

    #[tokio::test]
    async fn second_register_already_existed_bumps_last_seen_only() -> Result<(), StoreError> {
        let store = InMemoryStore::default();

        let first = store
            .register_namespace("orders", NamespaceOrigin::WorkerMint)
            .await?;
        assert_eq!(first, MintOutcome::Created);
        let original = store
            .get_namespace("orders")
            .await?
            .expect("namespace must persist");

        // A different origin on re-register must NOT overwrite the recorded origin.
        let second = store
            .register_namespace("orders", NamespaceOrigin::Explicit)
            .await?;
        assert_eq!(second, MintOutcome::AlreadyExisted);

        let touched = store
            .get_namespace("orders")
            .await?
            .expect("namespace must persist");
        assert_eq!(touched.created_at, original.created_at);
        assert_eq!(touched.origin, NamespaceOrigin::WorkerMint);
        assert!(touched.last_seen >= original.last_seen);
        Ok(())
    }

    #[tokio::test]
    async fn put_namespace_is_idempotent_on_existing_name() -> Result<(), StoreError> {
        let store = InMemoryStore::default();
        let now = Utc
            .with_ymd_and_hms(2026, 6, 30, 12, 0, 0)
            .single()
            .expect("valid instant");

        let mut record = NamespaceRecord::new_minted("billing", NamespaceOrigin::Explicit, now);
        record.config.kind = Some("tenant".to_owned());

        let created = store.put_namespace(record.clone()).await?;
        assert_eq!(created, MintOutcome::Created);

        // A second put with a DIFFERENT record body must reconcile as
        // AlreadyExisted and must not overwrite the stored record wholesale.
        let mut replacement =
            NamespaceRecord::new_minted("billing", NamespaceOrigin::WorkerMint, now);
        replacement.config.kind = None;
        let again = store.put_namespace(replacement).await?;
        assert_eq!(again, MintOutcome::AlreadyExisted);

        let stored = store
            .get_namespace("billing")
            .await?
            .expect("namespace must persist");
        assert_eq!(stored.origin, NamespaceOrigin::Explicit);
        assert_eq!(stored.config.kind.as_deref(), Some("tenant"));
        Ok(())
    }

    #[tokio::test]
    async fn list_orders_by_created_at_then_name() -> Result<(), StoreError> {
        let store = InMemoryStore::default();
        let earlier = Utc
            .with_ymd_and_hms(2026, 6, 30, 12, 0, 0)
            .single()
            .expect("valid instant");
        let later = Utc
            .with_ymd_and_hms(2026, 6, 30, 13, 0, 0)
            .single()
            .expect("valid instant");

        // Two share `earlier` (tiebreak by name), one is `later`.
        store
            .put_namespace(NamespaceRecord::new_minted(
                "zeta",
                NamespaceOrigin::Explicit,
                earlier,
            ))
            .await?;
        store
            .put_namespace(NamespaceRecord::new_minted(
                "alpha",
                NamespaceOrigin::Explicit,
                earlier,
            ))
            .await?;
        store
            .put_namespace(NamespaceRecord::new_minted(
                "beta",
                NamespaceOrigin::Explicit,
                later,
            ))
            .await?;

        let listed: Vec<String> = store
            .list_namespaces()
            .await?
            .into_iter()
            .map(|record| record.name)
            .collect();

        assert_eq!(listed, vec!["alpha", "zeta", "beta"]);
        Ok(())
    }

    #[tokio::test]
    async fn get_returns_none_for_absent_name() -> Result<(), StoreError> {
        let store = InMemoryStore::default();
        assert!(store.get_namespace("missing").await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn deprecate_sets_state_and_is_idempotent() -> Result<(), StoreError> {
        let store = InMemoryStore::default();
        store
            .register_namespace("orders", NamespaceOrigin::WorkerMint)
            .await?;

        store.deprecate_namespace("orders").await?;
        let deprecated = store
            .get_namespace("orders")
            .await?
            .expect("namespace must persist");
        assert_eq!(deprecated.state, NamespaceState::Deprecated);

        // Deprecating again is a no-op, not an error.
        store.deprecate_namespace("orders").await?;
        let still = store
            .get_namespace("orders")
            .await?
            .expect("namespace must persist");
        assert_eq!(still.state, NamespaceState::Deprecated);

        // Deprecating an absent namespace is also an idempotent no-op.
        store.deprecate_namespace("never-seen").await?;
        assert!(store.get_namespace("never-seen").await?.is_none());
        Ok(())
    }
}
