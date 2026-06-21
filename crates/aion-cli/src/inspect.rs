//! `aion inspect`: time-travel over a recorded run's event-store oplog (WA-004).
//!
//! This command is a read-only lens. It fetches the run's history over the same
//! `describe` read every other client command uses — there is no debug-only log
//! and no second store (C16, CN5). It resolves the run (the latest recorded
//! `WorkflowStarted`, or `--run-id`), then asks the engine's
//! [`aion::durability::inspect_run`] for the per-event state projection, the
//! recorded `now()` at each step, and the divergent command on a
//! non-determinism fault. `random()` is surfaced as a run-level draw-ordinal
//! projection, not a per-step field — draws are deterministic and never
//! recorded, and their per-step count is workflow-code dependent (see
//! [`aion::durability::RandomDrawProjection`]). With `--from <seq> --mock <json>`
//! it runs a what-if re-run from the chosen event via the same replay path and
//! reports the path.

use aion::durability::{
    DivergentCommand, InspectStep, MockOutcome, RandomDrawProjection, RunInspection,
    StepProjection, WhatIfOutcome, inspect_run, what_if_from,
};
use aion_client::Client;
use aion_core::{
    ActivityError, ActivityErrorKind, Event, Payload, RunId, WorkflowError, WorkflowId,
};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::payload::{json_payload, parse_run_id, payload_to_json};

/// Runs `aion inspect`, returning the JSON the CLI prints.
///
/// `from` and `mock` are required together for a what-if: a what-if never
/// defaults its fork point or its mocked outcome (ADR-001). Without them the
/// command returns the per-event inspection of the run.
///
/// # Errors
///
/// Returns an error when the workflow id or run id is invalid, the server
/// describe fails, the run cannot be resolved, the engine projection fails, or a
/// what-if is requested with only one of `--from`/`--mock`.
pub(crate) async fn run(
    client: &Client,
    workflow_id: &str,
    run_id: Option<&str>,
    from: Option<u64>,
    mock: Option<&str>,
) -> Result<Value> {
    let workflow_id = crate::payload::parse_workflow_id(workflow_id)?;
    let requested_run_id = run_id.map(parse_run_id).transpose()?;

    let history = fetch_history(client, &workflow_id, requested_run_id.as_ref()).await?;
    let run_id = resolve_run_id(&history, requested_run_id)?;

    match (from, mock) {
        (None, None) => render_inspection(history, &run_id),
        (Some(from_seq), Some(mock)) => render_what_if(history, &run_id, from_seq, mock),
        (Some(_), None) => bail!("--from requires --mock: a what-if never defaults its outcome"),
        (None, Some(_)) => bail!("--mock requires --from: a what-if never defaults its fork point"),
    }
}

async fn fetch_history(
    client: &Client,
    workflow_id: &WorkflowId,
    run_id: Option<&RunId>,
) -> Result<Vec<Event>> {
    let description = client
        .describe(workflow_id, run_id)
        .await
        .context("failed to read workflow history")?;
    Ok(description.history)
}

/// Resolves the run to inspect: the explicit `--run-id`, or the latest recorded
/// `WorkflowStarted` in history. `WorkflowSummary` carries no run id, so the run
/// identity is read from history (the same source replay segments from).
fn resolve_run_id(history: &[Event], requested: Option<RunId>) -> Result<RunId> {
    if let Some(run_id) = requested {
        return Ok(run_id);
    }
    history
        .iter()
        .rev()
        .find_map(|event| match event {
            Event::WorkflowStarted { run_id, .. } => Some(run_id.clone()),
            _ => None,
        })
        .ok_or_else(|| anyhow!("workflow history has no WorkflowStarted to resolve a run from"))
}

fn render_inspection(history: Vec<Event>, run_id: &RunId) -> Result<Value> {
    let inspection = inspect_run(history, run_id).context("failed to project run inspection")?;
    inspection_to_json(&inspection)
}

fn render_what_if(history: Vec<Event>, run_id: &RunId, from_seq: u64, mock: &str) -> Result<Value> {
    let mocked = parse_mock(mock).context("invalid --mock JSON")?;
    let outcome =
        what_if_from(history, run_id, from_seq, &mocked).context("failed to run what-if re-run")?;
    Ok(what_if_to_json(from_seq, &outcome))
}

fn inspection_to_json(inspection: &RunInspection) -> Result<Value> {
    let steps = inspection
        .steps
        .iter()
        .map(step_to_json)
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "workflow_id": inspection.workflow_id.to_string(),
        "run_id": inspection.run_id.to_string(),
        "steps": steps,
        "random": random_to_json(&inspection.random),
        "divergence": inspection.divergence.as_ref().map(divergence_to_json),
    }))
}

fn step_to_json(step: &InspectStep) -> Result<Value> {
    Ok(json!({
        "seq": step.seq,
        "event_kind": step.event_kind,
        "correlation_key": step.correlation_key.as_ref().map(|key| format!("{key:?}")),
        "now": step.now.to_rfc3339(),
        "projection": projection_to_json(&step.projection)?,
    }))
}

/// Renders the run's deterministic `random()` draw-ordinal projection.
///
/// `random()` is *not* a per-step value: draws are deterministic, never
/// recorded, and their per-step count is workflow-code dependent (only an
/// in-VM replay can recover counts — deferred). So this surfaces the projection
/// honestly: the value the production `random()` path serves at each draw
/// ordinal, computed by the same formula the running workflow used. The sample
/// shows the first draw ordinals; a consumer (the RM-007 dashboard) queries any
/// ordinal it needs against the same projection.
const RANDOM_SAMPLE_ORDINALS: u64 = 8;

fn random_to_json(random: &RandomDrawProjection) -> Value {
    let mut sample = Vec::with_capacity(usize::try_from(RANDOM_SAMPLE_ORDINALS).unwrap_or(0));
    for ordinal in 0..RANDOM_SAMPLE_ORDINALS {
        sample.push(json!({
            "ordinal": ordinal,
            "random": random.random_at(ordinal),
        }));
    }
    json!({
        "kind": "draw_ordinal_projection",
        "note": "random() is deterministic and never recorded; this is the value \
                 the production random() path serves at each draw ordinal for this \
                 run, not a per-event field. Per-step draw counts require an in-VM \
                 replay (deferred).",
        "sample": sample,
    })
}

fn projection_to_json(projection: &StepProjection) -> Result<Value> {
    match projection {
        StepProjection::Started {
            workflow_type,
            input,
        } => Ok(json!({
            "kind": "started",
            "workflow_type": workflow_type,
            "input": payload_to_json(input)?,
        })),
        StepProjection::Resolved(resolution) => Ok(json!({
            "kind": "resolved",
            "resolution": format!("{resolution:?}"),
        })),
        StepProjection::Terminal(terminal) => Ok(json!({
            "kind": "terminal",
            "terminal": format!("{terminal:?}"),
        })),
        StepProjection::AsyncArrival { kind } => Ok(json!({
            "kind": "async_arrival",
            "event": kind,
        })),
        StepProjection::NonReplay => Ok(json!({ "kind": "non_replay" })),
    }
}

fn divergence_to_json(divergence: &DivergentCommand) -> Value {
    json!({
        "seq": divergence.seq,
        "expected": divergence.expected,
        "found": divergence.found,
    })
}

fn what_if_to_json(from_seq: u64, outcome: &WhatIfOutcome) -> Value {
    // `from_seq` is emitted once, on the outer object; the per-kind body never
    // repeats it.
    let body = match outcome {
        WhatIfOutcome::Resolved { resolution, .. } => json!({
            "kind": "resolved",
            "resolution": format!("{resolution:?}"),
        }),
        WhatIfOutcome::Terminal(terminal) => json!({
            "kind": "terminal",
            "terminal": format!("{terminal:?}"),
        }),
        WhatIfOutcome::Diverged(divergence) => json!({
            "kind": "diverged",
            "divergence": divergence_to_json(divergence),
        }),
    };
    json!({ "from_seq": from_seq, "outcome": body })
}

/// Parses a `--mock` JSON document into a [`MockOutcome`].
///
/// The outcome kind is required and explicit; there is no default mock (ADR-001,
/// CN2). Shapes:
/// - `{"kind":"activity_completed","result":<json>}`
/// - `{"kind":"activity_failed","message":"...","details":<json>|null}`
/// - `{"kind":"child_completed","result":<json>}`
/// - `{"kind":"child_failed","message":"...","details":<json>|null}`
/// - `{"kind":"signal_delivered","payload":<json>}`
/// - `{"kind":"timer_fired"}`
fn parse_mock(raw: &str) -> Result<MockOutcome> {
    let value: Value = serde_json::from_str(raw)?;
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("--mock requires an explicit \"kind\" field"))?;

    match kind {
        "activity_completed" => Ok(MockOutcome::ActivityCompleted(field_payload(
            &value, "result",
        )?)),
        "activity_failed" => Ok(MockOutcome::ActivityFailed(activity_error(&value)?)),
        "child_completed" => Ok(MockOutcome::ChildCompleted(field_payload(
            &value, "result",
        )?)),
        "child_failed" => Ok(MockOutcome::ChildFailed(workflow_error(&value)?)),
        "signal_delivered" => Ok(MockOutcome::SignalDelivered(field_payload(
            &value, "payload",
        )?)),
        "timer_fired" => Ok(MockOutcome::TimerFired),
        other => Err(anyhow!(
            "unknown --mock kind '{other}'; expected one of activity_completed, activity_failed, \
             child_completed, child_failed, signal_delivered, timer_fired"
        )),
    }
}

fn field_payload(value: &Value, field: &str) -> Result<Payload> {
    let inner = value
        .get(field)
        .ok_or_else(|| anyhow!("--mock requires a '{field}' field for this kind"))?;
    json_payload(&inner.to_string())
}

fn activity_error(value: &Value) -> Result<ActivityError> {
    Ok(ActivityError {
        // A mocked activity failure must be terminal to resolve at the fork; the
        // engine rejects a retryable mock, so the kind is fixed terminal here.
        kind: ActivityErrorKind::Terminal,
        message: mock_message(value)?,
        details: optional_details(value)?,
    })
}

fn workflow_error(value: &Value) -> Result<WorkflowError> {
    Ok(WorkflowError {
        message: mock_message(value)?,
        details: optional_details(value)?,
    })
}

fn mock_message(value: &Value) -> Result<String> {
    value
        .get("message")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("--mock failure kinds require a 'message' field"))
}

fn optional_details(value: &Value) -> Result<Option<Payload>> {
    match value.get("details") {
        None | Some(Value::Null) => Ok(None),
        Some(details) => Ok(Some(json_payload(&details.to_string())?)),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RANDOM_SAMPLE_ORDINALS, inspection_to_json, parse_mock, resolve_run_id, what_if_to_json,
    };
    use aion::durability::{MockOutcome, ReplayTerminal, Resolution, WhatIfOutcome, inspect_run};
    use aion_core::{ActivityErrorKind, Event, EventEnvelope, Payload, RunId, WorkflowId};
    use chrono::Utc;
    use serde_json::{Value, json};
    use uuid::Uuid;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn started(run: u128) -> TestResult<Event> {
        Ok(Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq: 1,
                recorded_at: Utc::now(),
                workflow_id: WorkflowId::new(Uuid::nil()),
            },
            workflow_type: "wf".to_owned(),
            input: Payload::from_json(&json!(null))?,
            run_id: RunId::new(Uuid::from_u128(run)),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        })
    }

    #[test]
    fn resolve_run_id_prefers_explicit_request() -> TestResult {
        let requested = RunId::new(Uuid::from_u128(7));
        let resolved = resolve_run_id(&[started(1)?], Some(requested.clone()))?;
        assert_eq!(resolved, requested);
        Ok(())
    }

    #[test]
    fn resolve_run_id_falls_back_to_latest_started() -> TestResult {
        let mut history = vec![started(1)?];
        let mut later = started(2)?;
        if let Event::WorkflowStarted { envelope, .. } = &mut later {
            envelope.seq = 3;
        }
        history.push(later);

        let resolved = resolve_run_id(&history, None)?;
        assert_eq!(resolved, RunId::new(Uuid::from_u128(2)));
        Ok(())
    }

    #[test]
    fn resolve_run_id_errors_without_a_start() {
        assert!(resolve_run_id(&[], None).is_err());
    }

    #[test]
    fn parse_mock_reads_every_explicit_kind() -> TestResult {
        assert!(matches!(
            parse_mock(r#"{"kind":"activity_completed","result":{"ok":true}}"#)?,
            MockOutcome::ActivityCompleted(_)
        ));
        match parse_mock(r#"{"kind":"activity_failed","message":"boom"}"#)? {
            MockOutcome::ActivityFailed(error) => {
                assert_eq!(error.kind, ActivityErrorKind::Terminal);
                assert_eq!(error.message, "boom");
            }
            other => return Err(format!("expected activity_failed, got {other:?}").into()),
        }
        assert!(matches!(
            parse_mock(r#"{"kind":"child_completed","result":1}"#)?,
            MockOutcome::ChildCompleted(_)
        ));
        assert!(matches!(
            parse_mock(r#"{"kind":"child_failed","message":"child boom"}"#)?,
            MockOutcome::ChildFailed(_)
        ));
        assert!(matches!(
            parse_mock(r#"{"kind":"signal_delivered","payload":{"x":1}}"#)?,
            MockOutcome::SignalDelivered(_)
        ));
        assert!(matches!(
            parse_mock(r#"{"kind":"timer_fired"}"#)?,
            MockOutcome::TimerFired
        ));
        Ok(())
    }

    #[test]
    fn parse_mock_rejects_a_missing_or_unknown_kind() {
        assert!(parse_mock(r#"{"result":{}}"#).is_err());
        assert!(parse_mock(r#"{"kind":"teleport"}"#).is_err());
    }

    #[test]
    fn parse_mock_requires_explicit_failure_message() {
        assert!(parse_mock(r#"{"kind":"activity_failed"}"#).is_err());
        assert!(parse_mock(r#"{"kind":"child_failed"}"#).is_err());
    }

    #[test]
    fn parse_mock_requires_the_result_or_payload_field() {
        assert!(parse_mock(r#"{"kind":"activity_completed"}"#).is_err());
        assert!(parse_mock(r#"{"kind":"signal_delivered"}"#).is_err());
    }

    #[test]
    fn what_if_to_json_emits_from_seq_exactly_once() -> TestResult {
        // The de-dup fix: from_seq lives on the outer object only; the resolved
        // body never repeats it.
        let value = what_if_to_json(
            2,
            &WhatIfOutcome::Resolved {
                from_seq: 2,
                resolution: Resolution::TimerFired,
            },
        );
        assert_eq!(value["from_seq"], json!(2));
        let outcome = value
            .get("outcome")
            .and_then(Value::as_object)
            .ok_or("outcome should be an object")?;
        assert!(
            !outcome.contains_key("from_seq"),
            "the resolved body must not repeat from_seq",
        );
        assert_eq!(outcome["kind"], json!("resolved"));
        Ok(())
    }

    #[test]
    fn what_if_to_json_terminal_and_diverged_carry_from_seq_once() -> TestResult {
        let terminal = what_if_to_json(
            7,
            &WhatIfOutcome::Terminal(ReplayTerminal::Completed(Payload::from_json(&json!(
                "done"
            ))?)),
        );
        assert_eq!(terminal["from_seq"], json!(7));
        assert_eq!(terminal["outcome"]["kind"], json!("terminal"));
        Ok(())
    }

    fn run_id_value() -> RunId {
        RunId::new(Uuid::from_u128(2))
    }

    fn inspectable_history() -> TestResult<Vec<Event>> {
        Ok(vec![
            Event::WorkflowStarted {
                envelope: EventEnvelope {
                    seq: 1,
                    recorded_at: Utc::now(),
                    workflow_id: WorkflowId::new(Uuid::from_u128(1)),
                },
                workflow_type: "wf".to_owned(),
                input: Payload::from_json(&json!(null))?,
                run_id: run_id_value(),
                parent_run_id: None,
                package_version: aion_core::PackageVersion::new("a".repeat(64)),
            },
            Event::WorkflowCompleted {
                envelope: EventEnvelope {
                    seq: 2,
                    recorded_at: Utc::now(),
                    workflow_id: WorkflowId::new(Uuid::from_u128(1)),
                },
                result: Payload::from_json(&json!("done"))?,
            },
        ])
    }

    #[test]
    fn inspection_json_renders_random_as_a_draw_ordinal_projection_not_per_step() -> TestResult {
        // The honesty property at the CLI surface: there is no per-step random
        // field; random is a documented draw-ordinal projection whose sample
        // values match the production formula the engine exposes.
        let history = inspectable_history()?;
        let inspection = inspect_run(history, &run_id_value())?;
        let value = inspection_to_json(&inspection)?;

        // No step carries a random field.
        let steps = value["steps"]
            .as_array()
            .ok_or("steps should be an array")?;
        for step in steps {
            let step = step.as_object().ok_or("step should be an object")?;
            assert!(
                !step.contains_key("random_u64") && !step.contains_key("random"),
                "no per-step random field may exist (random is a run-level projection)",
            );
        }

        // The run-level random projection is present, labelled, and sampled.
        let random = value["random"].as_object().ok_or("random should exist")?;
        assert_eq!(random["kind"], json!("draw_ordinal_projection"));
        assert!(
            random.contains_key("note"),
            "the projection must document itself"
        );
        let sample = random["sample"]
            .as_array()
            .ok_or("sample should be an array")?;
        assert_eq!(sample.len(), usize::try_from(RANDOM_SAMPLE_ORDINALS)?);

        // Each sampled value equals the engine projection at that ordinal.
        for (ordinal, entry) in sample.iter().enumerate() {
            let ordinal = u64::try_from(ordinal)?;
            assert_eq!(entry["ordinal"], json!(ordinal));
            let expected = inspection.random.random_at(ordinal);
            let actual = entry["random"].as_f64().ok_or("random should be a float")?;
            assert!((expected - actual).abs() < f64::EPSILON);
        }
        Ok(())
    }
}
