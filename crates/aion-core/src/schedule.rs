//! Schedule identifiers, trigger specifications, and persisted schedule configuration.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Payload;

/// Identifier for a persisted schedule resource.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ScheduleId(Uuid);

impl ScheduleId {
    /// Creates a schedule identifier from an existing UUID.
    #[must_use]
    pub const fn new(id: Uuid) -> Self {
        Self(id)
    }

    /// Creates a schedule identifier with a random version 4 UUID.
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

impl fmt::Display for ScheduleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for ScheduleId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

/// Trigger definition for a persisted schedule.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq, Eq)]
pub enum TriggerSpec {
    /// Fire according to a cron expression.
    Cron {
        /// Cron expression to evaluate in later scheduling layers.
        expression: String,
    },
    /// Fire at a fixed interval.
    Interval {
        /// Duration between firings.
        #[ts(type = "{ secs: number, nanos: number }")]
        period: Duration,
    },
}

/// Policy for handling a schedule tick while an earlier run is still active.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, Default, PartialEq, Eq)]
pub enum OverlapPolicy {
    /// Skip ticks that overlap an active run.
    #[default]
    Skip,
    /// Keep at most one buffered tick while a run is active.
    BufferOne,
    /// Cancel the previous run before starting the new one.
    CancelPrevious,
    /// Allow every tick to start a run.
    AllowAll,
}

/// Policy for handling schedule ticks missed while the scheduler was unavailable.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, Default, PartialEq, Eq)]
pub enum CatchUpPolicy {
    /// Start every missed tick.
    All,
    /// Start one representative missed tick.
    #[default]
    One,
    /// Skip missed ticks.
    Skip,
}

/// Persisted configuration for a schedule resource.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq)]
pub struct ScheduleConfig {
    /// Trigger used to compute eligible fire times.
    pub trigger: TriggerSpec,
    /// Policy for overlapping firings.
    #[serde(default)]
    pub overlap_policy: OverlapPolicy,
    /// Policy for missed firings.
    #[serde(default)]
    pub catch_up_policy: CatchUpPolicy,
    /// Workflow type started when the schedule fires.
    pub workflow_type: String,
    /// Opaque workflow input supplied to triggered executions.
    pub input: Payload,
    /// Typed search attributes recorded on every triggered execution.
    #[serde(default)]
    pub search_attributes: std::collections::HashMap<String, crate::SearchAttributeValue>,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::time::Duration;

    use serde::de::DeserializeOwned;
    use serde_json::json;

    use super::{CatchUpPolicy, OverlapPolicy, ScheduleConfig, ScheduleId, TriggerSpec};
    use crate::Payload;

    fn round_trip<T>(value: &T) -> Result<(), serde_json::Error>
    where
        T: DeserializeOwned + PartialEq + serde::Serialize + std::fmt::Debug,
    {
        let json = serde_json::to_string(value)?;
        let decoded = serde_json::from_str::<T>(&json)?;
        assert_eq!(*value, decoded);
        Ok(())
    }

    fn payload(label: &str) -> Result<Payload, crate::PayloadError> {
        Payload::from_json(&json!({ "label": label }))
    }

    fn config() -> Result<ScheduleConfig, crate::PayloadError> {
        Ok(ScheduleConfig {
            trigger: TriggerSpec::Cron {
                expression: String::from("0 0 * * *"),
            },
            overlap_policy: OverlapPolicy::Skip,
            catch_up_policy: CatchUpPolicy::One,
            workflow_type: String::from("checkout"),
            input: payload("schedule-input")?,
            search_attributes: HashMap::from([(
                String::from("aion.namespace"),
                crate::SearchAttributeValue::String(String::from("tenant-a")),
            )]),
        })
    }

    #[test]
    fn schedule_id_round_trips_through_json() -> Result<(), Box<dyn std::error::Error>> {
        round_trip(&ScheduleId::new_v4())?;
        Ok(())
    }

    #[test]
    fn schedule_id_is_a_hash_map_key() {
        let schedule_id = ScheduleId::new_v4();

        let mut schedules = HashMap::new();
        schedules.insert(schedule_id.clone(), "schedule");
        assert_eq!(schedules.get(&schedule_id), Some(&"schedule"));
    }

    #[test]
    fn schedule_id_display_and_from_str_use_uuid_format() -> Result<(), uuid::Error> {
        let schedule_id = ScheduleId::new(uuid::Uuid::nil());

        assert_eq!(
            schedule_id.to_string(),
            "00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(
            ScheduleId::from_str("00000000-0000-0000-0000-000000000000")?,
            schedule_id
        );
        Ok(())
    }

    #[test]
    fn trigger_specs_round_trip_through_json() -> Result<(), Box<dyn std::error::Error>> {
        round_trip(&TriggerSpec::Cron {
            expression: String::from("*/5 * * * *"),
        })?;
        round_trip(&TriggerSpec::Interval {
            period: Duration::from_secs(300),
        })?;
        Ok(())
    }

    #[test]
    fn overlap_policies_round_trip_through_json() -> Result<(), serde_json::Error> {
        for policy in [
            OverlapPolicy::Skip,
            OverlapPolicy::BufferOne,
            OverlapPolicy::CancelPrevious,
            OverlapPolicy::AllowAll,
        ] {
            round_trip(&policy)?;
        }
        Ok(())
    }

    #[test]
    fn catch_up_policies_round_trip_through_json() -> Result<(), serde_json::Error> {
        for policy in [CatchUpPolicy::All, CatchUpPolicy::One, CatchUpPolicy::Skip] {
            round_trip(&policy)?;
        }
        Ok(())
    }

    #[test]
    fn schedule_config_round_trips_through_json() -> Result<(), Box<dyn std::error::Error>> {
        round_trip(&config()?)?;
        Ok(())
    }

    #[test]
    fn schedule_config_defaults_missing_policy_fields() -> Result<(), Box<dyn std::error::Error>> {
        let value = json!({
            "trigger": { "Cron": { "expression": "0 0 * * *" } },
            "workflow_type": "checkout",
            "input": payload("schedule-input")?,
        });

        let config = serde_json::from_value::<ScheduleConfig>(value)?;

        assert_eq!(config.overlap_policy, OverlapPolicy::Skip);
        assert_eq!(config.catch_up_policy, CatchUpPolicy::One);
        Ok(())
    }
}
