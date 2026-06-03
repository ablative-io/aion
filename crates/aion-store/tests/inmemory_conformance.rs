//! In-memory `EventStore` conformance coverage.

use std::sync::Arc;

use aion_store::{EventStore, InMemoryStore, StoreError, conformance::run_event_store_suite};

#[tokio::test]
async fn inmemory_store_satisfies_event_store_conformance_suite() -> Result<(), StoreError> {
    run_event_store_suite(|| async {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        store
    })
    .await
}
