//! AA-4-4: `EngineBuilder::bootstrap_schedule_coordinator` gates whether `build()`
//! seeds the schedule-coordinator history.
//!
//! Default (`true`) seeds it — single-node behavior, unchanged. A multi-node
//! deployment sets it `false` on every node that does NOT own the coordinator's
//! shard, so only the owner seeds (and serves) it. The assertion is id-agnostic:
//! a bare engine build with seeding on leaves exactly ONE workflow in the store
//! (the coordinator); with seeding off it leaves ZERO.

use std::sync::Arc;

use aion::EngineBuilder;
use aion_store::{EventStore, InMemoryStore};

#[tokio::test]
async fn bootstrap_flag_gates_schedule_coordinator_seeding() -> Result<(), Box<dyn std::error::Error>>
{
    // Default (true): the coordinator history is seeded at build.
    let seeded: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let _seeded_engine = EngineBuilder::new()
        .store_arc(Arc::clone(&seeded))
        .in_memory_visibility()
        .scheduler_threads(1)
        .build()
        .await?;
    let seeded_ids = seeded.list_workflow_ids().await?;
    assert_eq!(
        seeded_ids.len(),
        1,
        "default build must seed exactly the schedule coordinator, got {seeded_ids:?}"
    );

    // Disabled: a node that does not own the coordinator's shard seeds nothing.
    let bare: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let _bare_engine = EngineBuilder::new()
        .store_arc(Arc::clone(&bare))
        .in_memory_visibility()
        .scheduler_threads(1)
        .bootstrap_schedule_coordinator(false)
        .build()
        .await?;
    let bare_ids = bare.list_workflow_ids().await?;
    assert!(
        bare_ids.is_empty(),
        "build with bootstrap_schedule_coordinator(false) must seed no workflow, got {bare_ids:?}"
    );

    Ok(())
}
