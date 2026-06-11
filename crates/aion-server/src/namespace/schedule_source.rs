//! Durable schedule→namespace ownership sources.
//!
//! Schedule ownership is a projection of the schedule coordinator's durable
//! event history: the server force-stamps the authorized namespace into the
//! schedule config before the engine records `ScheduleCreated`, and ownership
//! is folded back out of the first `ScheduleCreated` event recorded for a
//! schedule id. Deriving from creation rather than the latest config makes
//! ownership immutable by construction — no update-path bug can ever migrate a
//! schedule between tenants. A `ScheduleCreated` whose config carries no
//! namespace attribute resolves to no owner and is therefore invisible through
//! every namespaced server API.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use aion::Engine;
use aion_core::{Event, ScheduleId, SearchAttributeValue};
use async_trait::async_trait;

use crate::error::ServerError;

use super::resolver::NAMESPACE_ATTRIBUTE;

/// Durable source of schedule→namespace ownership facts.
///
/// The production implementation projects ownership from the schedule
/// coordinator's recorded event history; tests substitute a static fixture to
/// prove adapter-boundary denials without an engine.
#[async_trait]
pub trait ScheduleNamespaceSource: Send + Sync {
    /// Returns the namespace recorded at schedule creation, or [`None`] when
    /// the schedule is unknown or its creation config recorded no namespace
    /// attribute.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when the underlying ownership data cannot be read.
    async fn schedule_namespace(
        &self,
        schedule_id: &ScheduleId,
    ) -> Result<Option<String>, ServerError>;
}

/// Production ownership source: folds the `aion.namespace` search attribute
/// out of the first `ScheduleCreated` event in the schedule coordinator's
/// durable history.
///
/// `ScheduleDeleted` deliberately does not erase ownership: a foreign probe of
/// a deleted schedule must still see the guard's anti-existence-leak `NotFound`,
/// while the owner's probe falls through to the engine's `ScheduleNotFound`.
pub(crate) struct HistoryScheduleNamespaceSource {
    engine: Arc<Engine>,
}

impl HistoryScheduleNamespaceSource {
    /// Build a source over the engine whose coordinator history records schedules.
    pub(crate) const fn new(engine: Arc<Engine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl ScheduleNamespaceSource for HistoryScheduleNamespaceSource {
    async fn schedule_namespace(
        &self,
        schedule_id: &ScheduleId,
    ) -> Result<Option<String>, ServerError> {
        // Read-amplification note: all schedule events share one coordinator
        // history, so this scan is O(global schedule history) per verification
        // rather than O(one schedule's events). Correct but a candidate for a
        // per-schedule visibility index once coordinator compaction lands.
        let history = self
            .engine
            .store()
            .read_history(self.engine.schedule_coordinator_workflow_id())
            .await
            .map_err(ServerError::from)?;
        for event in &history {
            if let Event::ScheduleCreated {
                schedule_id: created_id,
                config,
                ..
            } = event
                && created_id == schedule_id
            {
                // First ScheduleCreated wins: ownership is creation-pinned.
                return match config.search_attributes.get(NAMESPACE_ATTRIBUTE) {
                    Some(SearchAttributeValue::String(namespace)) => Ok(Some(namespace.clone())),
                    Some(other) => Err(ServerError::Config {
                        message: format!(
                            "schedule {schedule_id} recorded a non-string {NAMESPACE_ATTRIBUTE} search attribute: {other:?}"
                        ),
                    }),
                    None => Ok(None),
                };
            }
        }
        Ok(None)
    }
}

/// Static schedule→namespace fixture for adapter-boundary tests and alternate
/// wiring that must authorize without an engine handle.
#[derive(Clone, Default)]
pub struct StaticScheduleNamespaces {
    inner: Arc<RwLock<HashMap<ScheduleId, String>>>,
}

impl StaticScheduleNamespaces {
    /// Record that a schedule is owned by a namespace.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::LockPoisoned`] if the fixture lock was poisoned.
    pub fn record(&self, schedule_id: ScheduleId, namespace: &str) -> Result<(), ServerError> {
        let mut ownership = self
            .inner
            .write()
            .map_err(|_| ServerError::lock_poisoned("namespace schedule ownership"))?;
        ownership.insert(schedule_id, namespace.to_owned());
        Ok(())
    }
}

#[async_trait]
impl ScheduleNamespaceSource for StaticScheduleNamespaces {
    async fn schedule_namespace(
        &self,
        schedule_id: &ScheduleId,
    ) -> Result<Option<String>, ServerError> {
        let ownership = self
            .inner
            .read()
            .map_err(|_| ServerError::lock_poisoned("namespace schedule ownership"))?;
        Ok(ownership.get(schedule_id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use aion::{Engine, EngineBuilder};
    use aion_core::{
        CatchUpPolicy, Event, EventEnvelope, OverlapPolicy, Payload, ScheduleConfig, ScheduleId,
        SearchAttributeValue, TriggerSpec,
    };
    use aion_store::{EventStore, InMemoryStore, WriteToken, visibility::VisibilityStore};
    use chrono::Utc;
    use serde_json::json;

    use super::{
        HistoryScheduleNamespaceSource, NAMESPACE_ATTRIBUTE, ScheduleNamespaceSource,
        StaticScheduleNamespaces,
    };
    use crate::error::ServerError;

    struct Fixture {
        engine: Arc<Engine>,
        store: Arc<dyn EventStore>,
    }

    async fn fixture() -> Result<Fixture, aion::EngineError> {
        let backing = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = backing.clone();
        let visibility_store: Arc<dyn VisibilityStore> = backing;
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(Arc::clone(&store))
                .visibility_store_arc(visibility_store)
                .scheduler_threads(1)
                .build()
                .await?,
        );
        Ok(Fixture { engine, store })
    }

    fn schedule_config(
        attributes: HashMap<String, SearchAttributeValue>,
    ) -> Result<ScheduleConfig, aion_core::PayloadError> {
        Ok(ScheduleConfig {
            trigger: TriggerSpec::Interval {
                period: Duration::from_secs(60),
            },
            overlap_policy: OverlapPolicy::Skip,
            catch_up_policy: CatchUpPolicy::Skip,
            workflow_type: "fixture".to_owned(),
            input: Payload::from_json(&json!({ "fixture": true }))?,
            search_attributes: attributes,
        })
    }

    fn namespace_attributes(namespace: &str) -> HashMap<String, SearchAttributeValue> {
        HashMap::from([(
            NAMESPACE_ATTRIBUTE.to_owned(),
            SearchAttributeValue::String(namespace.to_owned()),
        )])
    }

    fn created_event(
        engine: &Engine,
        seq: u64,
        schedule_id: &ScheduleId,
        config: ScheduleConfig,
    ) -> Event {
        Event::ScheduleCreated {
            envelope: EventEnvelope {
                seq,
                recorded_at: Utc::now(),
                workflow_id: engine.schedule_coordinator_workflow_id().clone(),
            },
            schedule_id: schedule_id.clone(),
            config,
        }
    }

    /// Current coordinator history head: the engine builder seeds the
    /// coordinator workflow with its start event, so direct test appends must
    /// continue from the recorded head rather than zero.
    async fn coordinator_head(fixture: &Fixture) -> Result<u64, Box<dyn std::error::Error>> {
        let history = fixture
            .store
            .read_history(fixture.engine.schedule_coordinator_workflow_id())
            .await?;
        Ok(u64::try_from(history.len())?)
    }

    async fn append_coordinator_events(
        fixture: &Fixture,
        events: &[Event],
        expected_head: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        fixture
            .store
            .append(
                WriteToken::recorder(),
                fixture.engine.schedule_coordinator_workflow_id(),
                events,
                expected_head,
            )
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn history_source_reads_namespace_from_schedule_created()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture().await?;
        let head = coordinator_head(&fixture).await?;
        let schedule_id = ScheduleId::new(uuid::Uuid::from_u128(1));
        let event = created_event(
            &fixture.engine,
            head + 1,
            &schedule_id,
            schedule_config(namespace_attributes("tenant-a"))?,
        );
        append_coordinator_events(&fixture, std::slice::from_ref(&event), head).await?;
        let source = HistoryScheduleNamespaceSource::new(Arc::clone(&fixture.engine));

        assert_eq!(
            source.schedule_namespace(&schedule_id).await?,
            Some(String::from("tenant-a"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn history_source_returns_none_for_unknown_and_unstamped_schedules()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture().await?;
        let head = coordinator_head(&fixture).await?;
        let unstamped = ScheduleId::new(uuid::Uuid::from_u128(2));
        let unknown = ScheduleId::new(uuid::Uuid::from_u128(3));
        let event = created_event(
            &fixture.engine,
            head + 1,
            &unstamped,
            schedule_config(HashMap::new())?,
        );
        append_coordinator_events(&fixture, std::slice::from_ref(&event), head).await?;
        let source = HistoryScheduleNamespaceSource::new(Arc::clone(&fixture.engine));

        assert_eq!(source.schedule_namespace(&unstamped).await?, None);
        assert_eq!(source.schedule_namespace(&unknown).await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn history_source_rejects_non_string_namespace_attribute()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture().await?;
        let head = coordinator_head(&fixture).await?;
        let schedule_id = ScheduleId::new(uuid::Uuid::from_u128(4));
        let attributes =
            HashMap::from([(NAMESPACE_ATTRIBUTE.to_owned(), SearchAttributeValue::Int(7))]);
        let event = created_event(
            &fixture.engine,
            head + 1,
            &schedule_id,
            schedule_config(attributes)?,
        );
        append_coordinator_events(&fixture, std::slice::from_ref(&event), head).await?;
        let source = HistoryScheduleNamespaceSource::new(Arc::clone(&fixture.engine));

        let error = source.schedule_namespace(&schedule_id).await;

        assert!(matches!(error, Err(ServerError::Config { .. })));
        Ok(())
    }

    #[tokio::test]
    async fn ownership_is_pinned_to_creation_not_latest_update()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture().await?;
        let head = coordinator_head(&fixture).await?;
        let schedule_id = ScheduleId::new(uuid::Uuid::from_u128(5));
        let created = created_event(
            &fixture.engine,
            head + 1,
            &schedule_id,
            schedule_config(namespace_attributes("tenant-a"))?,
        );
        let updated = Event::ScheduleUpdated {
            envelope: EventEnvelope {
                seq: head + 2,
                recorded_at: Utc::now(),
                workflow_id: fixture.engine.schedule_coordinator_workflow_id().clone(),
            },
            schedule_id: schedule_id.clone(),
            config: schedule_config(namespace_attributes("tenant-b"))?,
        };
        append_coordinator_events(&fixture, &[created, updated], head).await?;
        let source = HistoryScheduleNamespaceSource::new(Arc::clone(&fixture.engine));

        assert_eq!(
            source.schedule_namespace(&schedule_id).await?,
            Some(String::from("tenant-a"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_creations_pin_the_first_recorded_owner()
    -> Result<(), Box<dyn std::error::Error>> {
        // Unreachable through public APIs (schedule ids are server-generated
        // v4 UUIDs), but the fold's lowest-sequence-wins rule is load-bearing
        // for ownership immutability, so pin it explicitly: a second
        // ScheduleCreated for the same id must never migrate the owner.
        let fixture = fixture().await?;
        let head = coordinator_head(&fixture).await?;
        let schedule_id = ScheduleId::new(uuid::Uuid::from_u128(8));
        let first = created_event(
            &fixture.engine,
            head + 1,
            &schedule_id,
            schedule_config(namespace_attributes("tenant-a"))?,
        );
        let second = created_event(
            &fixture.engine,
            head + 2,
            &schedule_id,
            schedule_config(namespace_attributes("tenant-b"))?,
        );
        append_coordinator_events(&fixture, &[first, second], head).await?;
        let source = HistoryScheduleNamespaceSource::new(Arc::clone(&fixture.engine));

        assert_eq!(
            source.schedule_namespace(&schedule_id).await?,
            Some(String::from("tenant-a"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn deleted_schedules_keep_their_recorded_owner() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = fixture().await?;
        let head = coordinator_head(&fixture).await?;
        let schedule_id = ScheduleId::new(uuid::Uuid::from_u128(6));
        let created = created_event(
            &fixture.engine,
            head + 1,
            &schedule_id,
            schedule_config(namespace_attributes("tenant-a"))?,
        );
        let deleted = Event::ScheduleDeleted {
            envelope: EventEnvelope {
                seq: head + 2,
                recorded_at: Utc::now(),
                workflow_id: fixture.engine.schedule_coordinator_workflow_id().clone(),
            },
            schedule_id: schedule_id.clone(),
        };
        append_coordinator_events(&fixture, &[created, deleted], head).await?;
        let source = HistoryScheduleNamespaceSource::new(Arc::clone(&fixture.engine));

        assert_eq!(
            source.schedule_namespace(&schedule_id).await?,
            Some(String::from("tenant-a"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn static_source_reports_recorded_namespace() -> Result<(), Box<dyn std::error::Error>> {
        let ownership = StaticScheduleNamespaces::default();
        let schedule_id = ScheduleId::new(uuid::Uuid::from_u128(7));
        ownership.record(schedule_id.clone(), "tenant-a")?;

        assert_eq!(
            ownership.schedule_namespace(&schedule_id).await?,
            Some(String::from("tenant-a"))
        );
        Ok(())
    }
}
