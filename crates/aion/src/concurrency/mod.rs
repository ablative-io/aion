//! concurrency module declarations + re-exports

/// Spawn a fixed set of child workflows and collect every result.
pub mod all;
/// Correlation tokens and mailboxes for child-workflow outcomes.
pub mod correlation;
/// Dynamic fan-out from runtime input collections.
pub mod map;
/// Spawn competing child workflows and return the first winner.
pub mod race;

pub use all::{AllChildWorkflowSpec, AllError, AllRecordingContext, all};
pub use correlation::{
    CancellationRecordingContext, CorrelatedOutcome, CorrelatedResult, CorrelatedResultTable,
    CorrelatedSlotState, CorrelationBatch, CorrelationError, CorrelationMailbox, CorrelationToken,
    InFlightChild, LinkedChild, SpawnSlot, VecCorrelationMailbox, cancel_remaining, derive_batch,
};
pub use map::{child_specs_from_items, map};
pub use race::{RaceChildSpec, RaceError, RaceRecordingContext, RaceWinner, race};
