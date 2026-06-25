//! Single-node `HaematiteStore` `EventStore` conformance coverage.
//!
//! Gates the haematite-backed store against the exact same behavioural suite the
//! in-memory and libSQL stores run (`aion_store::conformance`). Each scenario
//! gets a fresh temp-directory database.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aion_store::{EventStore, StoreError, conformance::run_event_store_suite};
use aion_store_haematite::HaematiteStore;

static DATABASE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[tokio::test(flavor = "multi_thread")]
async fn haematite_store_satisfies_event_store_conformance_suite() -> Result<(), StoreError> {
    run_event_store_suite(|| async {
        let store = HaematiteStore::create(unique_temp_dir("conformance"))
            .expect("create single-node haematite store");
        Arc::new(store) as Arc<dyn EventStore>
    })
    .await
}

fn unique_temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let counter = DATABASE_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "aion-store-haematite-{name}-{}-{nanos}-{counter}",
        std::process::id()
    ))
}
