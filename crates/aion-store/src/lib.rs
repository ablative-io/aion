//! The EventStore persistence contract, StoreError, the InMemoryStore reference implementation, and the shared behavioural conformance suite. Leaf crate (depends only on aion-core).

pub mod error;
pub mod memory;
pub mod store;
pub mod timer;
