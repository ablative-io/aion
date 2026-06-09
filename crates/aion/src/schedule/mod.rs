//! Schedule trigger parsing, state projection, policy, and timer-driven evaluation.

/// Timer-driven schedule evaluation orchestration.
pub mod evaluator;
/// Catch-up and overlap policy evaluation.
pub mod policy;
/// Schedule state projection from durable events.
pub mod state;
/// Cron parsing and next-fire-time calculation.
pub mod trigger;

pub use evaluator::{
    NoopScheduleCanceller, ScheduleEvaluator, ScheduleEvaluatorError, ScheduleEventSink,
    ScheduleEventSource, ScheduleTimer, ScheduleWorkflowCanceller, ScheduleWorkflowStarter,
    StoreScheduleTimer, TimerEvaluationOutcome,
};
pub use policy::{CatchUpPlan, OverlapDecision, evaluate_catch_up, evaluate_overlap};
pub use state::{ScheduleExecution, ScheduleState, project_schedule_state};
pub use trigger::{ScheduleError, next_fire_time, parse_cron_expression};
