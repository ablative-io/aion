//! `TimerEntry` and timer-facing types.

use aion_core::{TimerId, WorkflowId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Durable timer record returned by [`crate::EventStore::expired_timers`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub struct TimerEntry {
    /// Workflow that owns the timer.
    pub workflow_id: WorkflowId,
    /// Timer identifier within the owning workflow.
    pub timer_id: TimerId,
    /// Instant at which the timer is due to fire.
    pub fire_at: DateTime<Utc>,
}
