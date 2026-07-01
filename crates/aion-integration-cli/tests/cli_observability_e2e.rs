//! REAL end-to-end test of the observability-only [`CliHarness`]: spawn an actual line-oriented
//! process and drive one full activity attempt through the [`AgentHarness`] seam — exercising the
//! REAL adapter code path (spawn → stdout demux → neutral [`ActivityEvent`]s), not a stub returning
//! canned events.
//!
//! The "agent" is a deterministic in-crate fake: `sh -c` printing a fixed interleaved transcript
//! (a plain log line, a JSON message line, a JSON tool-call line, and a terminal stop line). It is
//! deterministic and needs no third-party binary, but the FULL adapter path runs against it: the
//! harness spawns the process, the session's pump reads its real stdout, and the demux maps each
//! real line into a neutral event.
//!
//! This is NOI-8 gate (1): the observability-only adapter streams a LIVE transcript with **NO
//! controls offered** (an empty-capability integration is first-class).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use aion_integration_cli::CliHarness;
use aion_integrations::{
    ActivityEventKind, ActivityId, AgentHarness, AgentRunSpec, AgentSession, ContentType,
    InterventionCommand, InterventionKind, Payload, WorkflowId,
};
use chrono::Utc;
use futures::StreamExt;

/// A deterministic fake CLI agent: prints a fixed interleaved transcript, then exits 0.
///
/// Mixes a free-form log line (→ `Raw`) with structured JSON lines (→ mapped kinds) exactly like a
/// real observability-only CLI, so the demux path is exercised for both the mapped and the
/// passthrough branch.
const FAKE_AGENT_SCRIPT: &str = "\
printf '%s\\n' '[info] starting the run';\
printf '%s\\n' '{\"type\":\"message\",\"text\":\"hello from the fake cli agent\"}';\
printf '%s\\n' '{\"type\":\"tool_call\",\"tool\":\"search\",\"call_id\":\"c1\",\"input\":{\"q\":\"x\"}}';\
printf '%s\\n' '{\"type\":\"tool_result\",\"call_id\":\"c1\",\"output\":{\"hits\":2}}';\
printf '%s\\n' '{\"type\":\"stop\",\"reason\":\"end_turn\"}';\
printf '%s\\n' 'the final answer is 42';\
exit 0";

/// A harness that runs the fake agent via `sh -c <script>`.
fn fake_cli_harness() -> CliHarness {
    CliHarness::new("sh")
        .with_arg("-c")
        .with_arg(FAKE_AGENT_SCRIPT)
}

fn spec() -> AgentRunSpec {
    AgentRunSpec::new(
        WorkflowId::new_v4(),
        ActivityId::from_sequence_position(1),
        1,
        Payload::new(ContentType::Json, b"run the task".to_vec()),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_harness_streams_a_live_transcript_with_no_controls() {
    let harness = fake_cli_harness();
    let mut session = harness
        .start(spec())
        .await
        .expect("spawn the fake cli agent");

    // (gate 1) NO controls offered — the observability-only session advertises an empty set.
    assert!(
        session.capabilities().is_empty(),
        "the observability-only harness offers NO intervention controls"
    );

    // Any command is rejected: there is no control channel at all.
    let cmd = InterventionCommand {
        workflow_id: WorkflowId::new_v4(),
        activity_id: ActivityId::from_sequence_position(1),
        attempt: 1,
        issued_by: None,
        issued_at: Utc::now(),
        kind: InterventionKind::Cancel {
            reason: "try to cancel".to_owned(),
        },
    };
    let rejected = session.intervene(cmd).await;
    assert!(
        matches!(
            rejected,
            Err(aion_integrations::HarnessError::CapabilityNotSupported { .. })
        ),
        "an observability-only session rejects every command, got {rejected:?}"
    );

    // A LIVE transcript streams from the real process stdout, demuxed into neutral events.
    let mut events = session.events();
    let collected = tokio::time::timeout(Duration::from_secs(30), async {
        let mut out = Vec::new();
        while let Some(event) = events.next().await {
            let is_stop = matches!(event.kind, ActivityEventKind::Stop { .. });
            out.push(event);
            if is_stop {
                // Keep draining a moment for the trailing final line, then stop.
                if let Ok(Some(tail)) =
                    tokio::time::timeout(Duration::from_secs(2), events.next()).await
                {
                    out.push(tail);
                }
                break;
            }
        }
        out
    })
    .await
    .expect("the transcript streams within the timeout");
    drop(events);

    // The demux produced the expected mix: a Raw log line, a mapped Message, ToolCall, ToolResult,
    // and a terminal Stop — proving the REAL demux ran end-to-end over real stdout.
    assert!(
        collected
            .iter()
            .any(|e| matches!(e.kind, ActivityEventKind::Raw { .. })),
        "the plain log line demuxes to Raw"
    );
    assert!(
        collected
            .iter()
            .any(|e| matches!(e.kind, ActivityEventKind::Message { .. })),
        "the JSON message line demuxes to Message"
    );
    assert!(
        collected
            .iter()
            .any(|e| matches!(e.kind, ActivityEventKind::ToolCall { .. })),
        "the JSON tool_call line demuxes to ToolCall"
    );
    assert!(
        collected
            .iter()
            .any(|e| matches!(e.kind, ActivityEventKind::ToolResult { .. })),
        "the JSON tool_result line demuxes to ToolResult"
    );
    assert!(
        collected
            .iter()
            .any(|e| matches!(e.kind, ActivityEventKind::Stop { .. })),
        "the JSON stop line demuxes to Stop"
    );

    // The terminal result is captured from the real process (final line + exit 0).
    let payload = tokio::time::timeout(Duration::from_secs(30), session.wait_result())
        .await
        .expect("wait_result resolves")
        .expect("a clean exit yields a result payload");
    assert_eq!(payload.content_type(), &ContentType::Json);
    let decoded = payload.to_json().expect("the result is JSON");
    assert_eq!(decoded["result"], serde_json::json!("completed"));
    assert_eq!(
        decoded["output"],
        serde_json::json!("the final answer is 42")
    );
    assert_eq!(decoded["exit_code"], serde_json::json!(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_harness_reports_a_non_zero_exit_as_a_harness_failure() {
    // A fake agent that prints one line then fails: the non-zero exit is an honest harness failure,
    // not a silent success.
    let harness = CliHarness::new("sh")
        .with_arg("-c")
        .with_arg("printf '%s\\n' '{\"type\":\"message\",\"text\":\"boom\"}'; exit 3");
    let mut session = harness
        .start(spec())
        .await
        .expect("spawn the fake cli agent");
    let _events: Vec<_> = session.events().collect().await;
    let result = tokio::time::timeout(Duration::from_secs(30), session.wait_result())
        .await
        .expect("wait_result resolves");
    assert!(
        matches!(result, Err(aion_integrations::HarnessError::Harness { .. })),
        "a non-zero exit surfaces as a harness failure, got {result:?}"
    );
}
