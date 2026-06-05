//! Strongly typed identifiers for workflows, activities, timers, and runs.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identifier for a logical workflow.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq, Hash)]
pub struct WorkflowId(Uuid);

impl WorkflowId {
    /// Creates a workflow identifier from an existing UUID.
    #[must_use]
    pub const fn new(id: Uuid) -> Self {
        Self(id)
    }

    /// Creates a workflow identifier with a random version 4 UUID.
    #[must_use]
    pub fn new_v4() -> Self {
        Self(Uuid::new_v4())
    }

    /// Returns the UUID backing this identifier.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Display for WorkflowId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Identifier for an activity scheduled within a workflow history.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ActivityId(u64);

impl ActivityId {
    /// Derives an activity identifier from its scheduling sequence position.
    #[must_use]
    pub const fn from_sequence_position(sequence_position: u64) -> Self {
        Self(sequence_position)
    }

    /// Returns the scheduling sequence position used to derive this identifier.
    #[must_use]
    pub const fn sequence_position(&self) -> u64 {
        self.0
    }
}

impl fmt::Display for ActivityId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "activity:{}", self.0)
    }
}

/// Errors from identifier construction.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum IdError {
    /// A timer name must be non-empty.
    #[error("timer name must not be empty")]
    EmptyTimerName,
}

/// Identifier for a timer scheduled by workflow code or by the engine.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq, Hash)]
pub struct TimerId(TimerIdKind);

impl TimerId {
    /// Creates an author-assigned timer identifier.
    ///
    /// # Errors
    ///
    /// Returns [`IdError::EmptyTimerName`] if the name is empty.
    pub fn named(name: impl Into<String>) -> Result<Self, IdError> {
        let name = name.into();
        if name.is_empty() {
            return Err(IdError::EmptyTimerName);
        }
        Ok(Self(TimerIdKind::Named(name)))
    }

    /// Creates an engine-assigned timer identifier derived from sequence position.
    #[must_use]
    pub const fn anonymous(sequence_position: u64) -> Self {
        Self(TimerIdKind::Anonymous(sequence_position))
    }

    /// Returns the author-assigned timer name, if this is a named timer.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match &self.0 {
            TimerIdKind::Named(name) => Some(name.as_str()),
            TimerIdKind::Anonymous(_) => None,
        }
    }

    /// Returns the scheduling sequence position, if this is an anonymous timer.
    #[must_use]
    pub fn sequence_position(&self) -> Option<u64> {
        match &self.0 {
            TimerIdKind::Named(_) => None,
            TimerIdKind::Anonymous(sequence_position) => Some(*sequence_position),
        }
    }
}

impl fmt::Display for TimerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            TimerIdKind::Named(name) => write!(formatter, "timer:named:{name}"),
            TimerIdKind::Anonymous(sequence_position) => {
                write!(formatter, "timer:anonymous:{sequence_position}")
            }
        }
    }
}

/// Backing representation for timer identifiers.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq, Hash)]
enum TimerIdKind {
    /// Author-assigned timer name.
    Named(String),
    /// Engine-assigned timer sequence position.
    Anonymous(u64),
}

/// Identifier for a concrete run of a logical workflow.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq, Hash)]
pub struct RunId(Uuid);

impl RunId {
    /// Creates a run identifier from an existing UUID.
    #[must_use]
    pub const fn new(id: Uuid) -> Self {
        Self(id)
    }

    /// Creates a run identifier with a random version 4 UUID.
    #[must_use]
    pub fn new_v4() -> Self {
        Self(Uuid::new_v4())
    }

    /// Returns the UUID backing this identifier.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde::de::DeserializeOwned;

    use super::{ActivityId, IdError, RunId, TimerId, WorkflowId};

    fn round_trip<T>(identifier: &T) -> Result<(), serde_json::Error>
    where
        T: DeserializeOwned + PartialEq + serde::Serialize + std::fmt::Debug,
    {
        let json = serde_json::to_string(identifier)?;
        let decoded = serde_json::from_str::<T>(&json)?;
        assert_eq!(*identifier, decoded);
        Ok(())
    }

    #[test]
    fn identifiers_round_trip_through_json() -> Result<(), Box<dyn std::error::Error>> {
        round_trip(&WorkflowId::new_v4())?;
        round_trip(&ActivityId::from_sequence_position(17))?;
        round_trip(&TimerId::named("reminder")?)?;
        round_trip(&TimerId::anonymous(29))?;
        round_trip(&RunId::new_v4())?;
        Ok(())
    }

    #[test]
    fn uuid_identifiers_are_hash_map_keys() {
        let workflow_id = WorkflowId::new_v4();
        let run_id = RunId::new_v4();

        let mut workflows = HashMap::new();
        workflows.insert(workflow_id.clone(), "workflow");
        assert_eq!(workflows.get(&workflow_id), Some(&"workflow"));

        let mut runs = HashMap::new();
        runs.insert(run_id.clone(), "run");
        assert_eq!(runs.get(&run_id), Some(&"run"));
    }

    #[test]
    fn sequence_identifiers_expose_positions() -> Result<(), IdError> {
        let activity_id = ActivityId::from_sequence_position(42);
        let timer_id = TimerId::anonymous(43);

        assert_eq!(activity_id.sequence_position(), 42);
        assert_eq!(timer_id.sequence_position(), Some(43));
        assert_eq!(TimerId::named("deadline")?.name(), Some("deadline"));
        Ok(())
    }

    #[test]
    fn display_formats_are_stable() -> Result<(), IdError> {
        let workflow_id = WorkflowId::new(uuid::Uuid::nil());
        let run_id = RunId::new(uuid::Uuid::nil());

        assert_eq!(
            workflow_id.to_string(),
            "00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(run_id.to_string(), "00000000-0000-0000-0000-000000000000");
        assert_eq!(
            ActivityId::from_sequence_position(7).to_string(),
            "activity:7"
        );
        assert_eq!(
            TimerId::named("reminder")?.to_string(),
            "timer:named:reminder"
        );
        assert_eq!(TimerId::anonymous(3).to_string(), "timer:anonymous:3");
        Ok(())
    }

    #[test]
    fn named_timer_rejects_empty_name() {
        assert_eq!(TimerId::named(""), Err(IdError::EmptyTimerName));
    }
}
