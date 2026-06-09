//! pub mod declarations + re-exports only

/// Durable command model and resolution outcomes.
pub mod command;
/// Correlation keys used to match commands to recorded events.
pub mod correlation;
/// History cursor used during replay resolution.
pub mod cursor;
/// Deterministic time and randomness context.
pub mod determinism;
/// Durability and non-determinism errors.
pub mod error;
/// Live execution handoff after replay is exhausted.
pub mod executor;
/// Single-writer recorder for workflow history events.
pub mod recorder;
/// Active workflow recovery and replay orchestration.
pub mod recovery;
/// Replay driver and replay-step outcomes.
pub mod replay;
/// Command resolver that compares live commands with recorded history.
pub mod resolver;
/// Event sequence-head helpers.
pub mod seq;
/// Signal delivery state for durable signal handling.
pub mod signal;

pub use command::{Command, Resolution, ResolveOutcome};
pub use correlation::CorrelationKey;
pub use cursor::{CursorResolveResult, FoundEventDescriptor, HistoryCursor, RecordedEventFamily};
pub use determinism::DeterminismContext;
pub use error::{DurabilityError, NonDeterminismError};
pub use executor::{
    HandoffOutcome, LiveActivityOutcome, LiveChildOutcome, LiveExecutor, resolve_or_execute_live,
};
pub use recorder::Recorder;
pub use recovery::{
    ActiveWorkflowRecovery, ActiveWorkflowRecoverySeam, DeferredActiveWorkflowRecovery,
    RecoveryDriver, RecoveryOutcome, RecoveryPlan, RecoveryReport, RecoveryResumePoint, recover,
};
pub use replay::{Replay, ReplayOutcome, ReplayStep, ReplayTerminal};
pub use resolver::{
    NON_DETERMINISM_WORKFLOW_ERROR_PREFIX, ResolvedCommand, Resolver, fail_on_violation,
};
pub use seq::SequenceHead;
pub use signal::SignalDelivery;
