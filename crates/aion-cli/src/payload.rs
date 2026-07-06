//! Payload and argument parsing helpers for the CLI.

use aion_core::{ContentType, Payload, RunId, WorkflowId, WorkflowStatus};
use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

pub(crate) fn json_payload(raw: &str) -> Result<Payload> {
    let value: Value = serde_json::from_str(raw)?;
    Payload::from_json(&value).context("failed to serialize JSON payload")
}

pub(crate) fn empty_query_payload() -> Payload {
    Payload::new(ContentType::Json, Vec::new())
}

pub(crate) fn payload_to_json(payload: &Payload) -> Result<Value> {
    payload
        .to_json()
        .context("query result was not a valid JSON payload")
}

pub(crate) fn parse_workflow_id(raw: &str) -> Result<WorkflowId> {
    let uuid = Uuid::parse_str(raw).with_context(|| format!("invalid workflow id '{raw}'"))?;
    Ok(WorkflowId::new(uuid))
}

pub(crate) fn parse_run_id(raw: &str) -> Result<RunId> {
    let uuid = Uuid::parse_str(raw).with_context(|| format!("invalid run id '{raw}'"))?;
    Ok(RunId::new(uuid))
}

pub(crate) fn parse_status(raw: &str) -> Result<WorkflowStatus, String> {
    match status_key(raw).as_str() {
        "running" => Ok(WorkflowStatus::Running),
        "completed" => Ok(WorkflowStatus::Completed),
        "failed" => Ok(WorkflowStatus::Failed),
        "cancelled" | "canceled" => Ok(WorkflowStatus::Cancelled),
        "timedout" | "timed-out" => Ok(WorkflowStatus::TimedOut),
        "continuedasnew" | "continued-as-new" => Ok(WorkflowStatus::ContinuedAsNew),
        "paused" => Ok(WorkflowStatus::Paused),
        _ => Err(String::from(
            "expected one of running, completed, failed, cancelled, timed-out, continued-as-new",
        )),
    }
}

fn status_key(raw: &str) -> String {
    raw.trim()
        .chars()
        .map(|character| match character {
            '_' | ' ' => '-',
            other => other.to_ascii_lowercase(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{json_payload, parse_run_id, parse_status, parse_workflow_id, payload_to_json};
    use aion_core::WorkflowStatus;

    #[test]
    fn json_payload_round_trips_to_json_value() -> anyhow::Result<()> {
        let payload = json_payload(r#"{"name":"Ada"}"#)?;

        assert_eq!(payload_to_json(&payload)?, json!({ "name": "Ada" }));
        Ok(())
    }

    #[test]
    fn parse_status_accepts_documented_and_serde_spellings() {
        assert_eq!(parse_status("running"), Ok(WorkflowStatus::Running));
        assert_eq!(parse_status("Completed"), Ok(WorkflowStatus::Completed));
        assert_eq!(parse_status("timed-out"), Ok(WorkflowStatus::TimedOut));
        assert_eq!(parse_status("TimedOut"), Ok(WorkflowStatus::TimedOut));
        assert_eq!(
            parse_status("continued_as_new"),
            Ok(WorkflowStatus::ContinuedAsNew)
        );
        assert_eq!(
            parse_status("ContinuedAsNew"),
            Ok(WorkflowStatus::ContinuedAsNew)
        );
    }

    #[test]
    fn parse_workflow_id_rejects_invalid_uuid() {
        assert!(parse_workflow_id("not-a-uuid").is_err());
    }

    #[test]
    fn parse_run_id_rejects_invalid_uuid() {
        assert!(parse_run_id("not-a-uuid").is_err());
    }
}
