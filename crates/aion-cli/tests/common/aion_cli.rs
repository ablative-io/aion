//! Thin `aion` CLI client helpers for OS-process gates: run a remote command and
//! parse its JSON, start a workflow, describe a run, and read the observables the
//! failover gate asserts on (status + per-ordinal terminal count).

use std::process::Command;

use serde_json::Value;

/// Run `aion <args>` and return parsed JSON stdout, or `None` on any failure (a
/// fenced start, an unreachable endpoint) so the caller can retry/poll.
pub fn aion_json(args: &[&str]) -> Option<Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_aion"))
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

/// Start the `collect_four` fan-out (workflow type = the fixture module name)
/// against `endpoint`, returning its workflow id on success.
pub fn try_start(endpoint: &str) -> Option<String> {
    let value = aion_json(&[
        "start",
        "aion_outbox_fixture",
        "--input",
        "{\"fixture\":\"input\"}",
        "--endpoint",
        endpoint,
    ])?;
    value
        .get("workflow_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Describe `workflow_id` over `endpoint`, returning the parsed JSON.
pub fn describe(endpoint: &str, workflow_id: &str) -> Option<Value> {
    aion_json(&["describe", workflow_id, "--endpoint", endpoint])
}

/// The `summary.status` string from a describe payload, if present.
pub fn status_of(description: &Value) -> Option<&str> {
    description
        .get("summary")
        .and_then(|summary| summary.get("status"))
        .and_then(Value::as_str)
}

/// Count `ActivityCompleted` terminal events in a describe payload's history —
/// the exactly-once observable. Events serialize as `{"type": "...", ...}`.
pub fn activity_completed_count(description: &Value) -> usize {
    description
        .get("history")
        .and_then(Value::as_array)
        .map_or(0, |events| {
            events
                .iter()
                .filter(|event| {
                    event.get("type").and_then(Value::as_str) == Some("ActivityCompleted")
                })
                .count()
        })
}
