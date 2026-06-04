//! concurrency module declarations + re-exports

pub mod all;
pub mod correlation;
pub mod map;
pub mod race;

pub use correlation::{
    CancellationRecordingContext, CorrelatedOutcome, CorrelatedResult, CorrelatedResultTable,
    CorrelatedSlotState, CorrelationBatch, CorrelationError, CorrelationMailbox, CorrelationToken,
    InFlightChild, LinkedChild, SpawnSlot, VecCorrelationMailbox, cancel_remaining, derive_batch,
};
