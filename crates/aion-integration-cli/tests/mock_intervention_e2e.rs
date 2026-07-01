//! REAL end-to-end test of the interveneable [`MockAgentHarness`] driven through the REAL worker
//! trait driver ([`aion_worker::spawn_agent`]) — NOI-8 gate (2).
//!
//! This drives the neutral intervention path the design specifies (operator command → server →
//! PUSH → worker → [`AgentSession::intervene`] → agent) at the worker leg that this crate can
//! exercise without a running server: a [`ControlMessage`] (the exact shape the server's
//! intervention PUSH lands on the worker's control channel) is delivered into a live
//! `spawn_agent` run, reaches [`MockAgentSession::intervene`], and its neutral
//! [`aion_core::InterventionOutcome`] ack returns to the caller. Crucially this uses the **real**
//! `spawn_agent` driver over a **real** `AgentHarness` (the mock), NOT a stub returning canned
//! outcomes.
//!
//! It asserts:
//!
//! 1. an advertised `InjectMessage` DRIVES through the neutral contract end-to-end and is `Applied`
//!    (and actually reaches the mock agent, recorded in its applied log),
//! 2. an advertised `Cancel` likewise drives through and is `Applied`, and
//! 3. an advertised-UNSUPPORTED `RespondToApproval` is cleanly REJECTED with a
//!    capability-not-supported NACK and NEVER reaches the agent.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use aion_core::{
    ActivityId, ApprovalDecision, InjectPriority, InterventionCommand, InterventionKind,
    InterventionOutcome, InterventionPrimitive, Payload, WorkflowId,
};
use aion_integration_cli::MockAgentHarness;
use aion_integrations::{AgentRunSpec, ContentType};
use aion_worker::{ControlMessage, DispatchOutcome, spawn_agent};
use chrono::Utc;
use tokio::sync::{mpsc, oneshot};

fn spec() -> AgentRunSpec {
    AgentRunSpec::new(
        WorkflowId::new(uuid::Uuid::nil()),
        ActivityId::from_sequence_position(3),
        1,
        Payload::new(ContentType::Json, b"\"run\"".to_vec()),
    )
}

fn command(kind: InterventionKind) -> InterventionCommand {
    InterventionCommand {
        workflow_id: WorkflowId::new(uuid::Uuid::nil()),
        activity_id: ActivityId::from_sequence_position(3),
        attempt: 1,
        issued_by: Some("operator".to_owned()),
        issued_at: Utc::now(),
        kind,
    }
}

/// Deliver one command onto the driver's control channel with an ack reply channel and await the
/// neutral outcome — the exact shape the server's PUSH lands on the worker.
async fn deliver(
    control_tx: &mpsc::UnboundedSender<ControlMessage>,
    kind: InterventionKind,
) -> InterventionOutcome {
    let (ack_tx, ack_rx) = oneshot::channel();
    control_tx
        .send(ControlMessage::with_ack(command(kind), ack_tx))
        .expect("the driver control channel is live");
    tokio::time::timeout(Duration::from_secs(10), ack_rx)
        .await
        .expect("the ack returns within the timeout")
        .expect("the driver replies an ack")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drives_inject_and_cancel_end_to_end_and_rejects_respond_to_approval() {
    let harness = MockAgentHarness::new();
    let applied = harness.applied();

    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (control_tx, control_rx) = mpsc::unbounded_channel();

    // Run the mock through the REAL worker driver, with the control channel the server would push
    // operator commands onto. Clone the harness so this task can end the run via its run handle
    // after the interventions are delivered.
    let run_harness = harness.clone();
    let driver =
        tokio::spawn(
            async move { spawn_agent(&run_harness, spec(), event_tx, Some(control_rx)).await },
        );

    // Wait until the run has started (its RunHandle is published) so commands land mid-run.
    let run_handle = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(handle) = harness.take_run_handle() {
                break handle;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the run starts and publishes its handle");

    // The run is live (its fixed transcript is streaming). Deliver interventions through the neutral
    // contract while it runs.

    // (1) an advertised InjectMessage drives end-to-end → Applied, and reaches the agent.
    let inject = deliver(
        &control_tx,
        InterventionKind::InjectMessage {
            text: "stop editing that file, use the other module".to_owned(),
            priority: InjectPriority::Interrupt,
        },
    )
    .await;
    assert_eq!(
        inject,
        InterventionOutcome::Applied,
        "an advertised InjectMessage drives through the neutral contract and applies"
    );

    // (2) an advertised Cancel likewise drives end-to-end → Applied.
    let cancel = deliver(
        &control_tx,
        InterventionKind::Cancel {
            reason: "operator abort".to_owned(),
        },
    )
    .await;
    assert_eq!(cancel, InterventionOutcome::Applied);

    // (3) an advertised-UNSUPPORTED RespondToApproval is NACKed capability-not-supported, and never
    //     reaches the agent.
    let approval = deliver(
        &control_tx,
        InterventionKind::RespondToApproval {
            call_id: "c1".to_owned(),
            decision: ApprovalDecision::Approve,
            note: None,
        },
    )
    .await;
    assert_eq!(
        approval,
        InterventionOutcome::capability_not_supported(InterventionPrimitive::RespondToApproval),
        "an unsupported primitive is cleanly NACKed, not crashed or applied"
    );

    // The two accepted commands reached the mock agent; the gated one did not.
    let applied_kinds = applied.lock().unwrap().clone();
    assert_eq!(
        applied_kinds.len(),
        2,
        "exactly the two advertised commands reached the agent: {applied_kinds:?}"
    );
    assert!(
        applied_kinds.iter().all(|k| matches!(
            k.primitive(),
            InterventionPrimitive::InjectMessage | InterventionPrimitive::Cancel
        )),
        "only inject_message/cancel were applied"
    );

    // End the run: drop the control channel (no more commands) and drop the run handle so the
    // mock's transcript stream ends and the driver takes the terminal result.
    drop(control_tx);
    run_handle.end_run();
    // Drain any remaining transcript events so the driver's event sink does not block it.
    while (tokio::time::timeout(Duration::from_secs(1), event_rx.recv()).await)
        .unwrap_or(None)
        .is_some()
    {}

    let outcome = tokio::time::timeout(Duration::from_secs(10), driver)
        .await
        .expect("the driver finishes")
        .expect("the driver task did not panic")
        .expect("the run produced a terminal outcome");
    assert!(
        matches!(outcome, DispatchOutcome::Completed { .. }),
        "the run completes with a replay-authoritative result, got {outcome:?}"
    );
}
