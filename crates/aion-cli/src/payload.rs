//! Payload and argument parsing helpers for the CLI.

use aion_core::{ContentType, Payload, WorkflowId, WorkflowStatus};
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

pub(crate) fn parse_status(raw: &str) -> Result<WorkflowStatus, String> {
    match raw {
        "Running" => Ok(WorkflowStatus::Running),
        "Completed" => Ok(WorkflowStatus::Completed),
        "Failed" => Ok(WorkflowStatus::Failed),
        "Cancelled" => Ok(WorkflowStatus::Cancelled),
        "TimedOut" => Ok(WorkflowStatus::TimedOut),
        "ContinuedAsNew" => Ok(WorkflowStatus::ContinuedAsNew),
        _ => Err(String::from(
            "expected one of Running, Completed, Failed, Cancelled, TimedOut, ContinuedAsNew",
        )),
    }
}
