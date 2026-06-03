//! Persistence contract for Aion event stores. Leaf crate depending only on `aion-core`.

pub mod error;
pub mod memory;
pub mod store;
pub mod timer;

pub use error::StoreError;
pub use store::EventStore;
pub use timer::TimerEntry;
