//! Mid-run intervention endpoint (NOI-6).
//!
//! `POST /workflows/intervene` submits a neutral
//! [`InterventionCommand`](aion_core::InterventionCommand) for a running activity
//! attempt (target identity in the body). It is **namespace-scoped** exactly like
//! `/workflows/signal` and
//! `/workflows/cancel` (a privileged mutating action): the caller must hold a grant
//! for the target `namespace` AND the target workflow must be visible in it, so a
//! caller cannot steer a foreign workflow. When auth is off it is granted by
//! default, consistent with the deploy-grant model.
//!
//! The endpoint gates + routes through [`ServerState::intervention_router`]: the
//! router refuses an unadvertised primitive at the server (never sending it),
//! resolves the owning worker, and pushes the command to it, returning the neutral
//! [`InterventionOutcome`] ack. The endpoint always returns `200 OK` with that
//! neutral ack body — a gated or stale-target outcome is a first-class ack the
//! operator inspects, NOT an HTTP error; only an authorization failure or a
//! malformed request is an HTTP error.

use aion_core::{
    ActivityId, InterventionCommand, InterventionKind, InterventionOutcome, WorkflowId,
};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

use super::auth::HttpCaller;
use super::error::HttpWireError;
use crate::namespace::WorkflowTarget;
use crate::{NamespaceOperation, ServerError, ServerState};

/// The intervention request body: the neutral command's identity + primitive plus
/// the namespace the target workflow runs under (the auth scope).
///
/// `issued_by`/`issued_at` are NOT accepted from the client — they are stamped by
/// the server from the authenticated caller and the receive instant, so the
/// transcript attribution cannot be forged.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct InterveneRequest {
    /// The namespace the target workflow runs under (the auth scope).
    pub namespace: String,
    /// The target workflow.
    pub workflow_id: WorkflowId,
    /// The target activity within the workflow.
    pub activity_id: ActivityId,
    /// The target attempt. A command to a stale/finished attempt is a no-op.
    pub attempt: u32,
    /// The neutral control primitive to apply.
    pub kind: InterventionKind,
}

/// The intervention response body: the neutral applied/gated/stale ack.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct InterveneResponse {
    /// The neutral outcome the operator inspects.
    pub outcome: InterventionOutcome,
}

/// `POST /workflows/intervene`.
///
/// Namespace-gates the caller, then routes the command through the intervention
/// router. Returns the neutral ack (applied / capability-not-supported /
/// stale-target). A gated or stale-target outcome is a `200 OK` ack, not an error;
/// only an authorization failure or a routing/lock fault is an HTTP error.
pub(crate) async fn intervene(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<InterveneRequest>,
) -> Result<Json<InterveneResponse>, HttpWireError> {
    let outcome = run_intervention(&state, &caller, request)
        .await
        .map_err(|error| HttpWireError(error.to_wire_error()))?;
    Ok(Json(InterveneResponse { outcome }))
}

/// Namespace-gate the caller and route the command, returning the neutral ack.
async fn run_intervention(
    state: &ServerState,
    caller: &crate::CallerIdentity,
    request: InterveneRequest,
) -> Result<InterventionOutcome, ServerError> {
    // Namespace-scope + durable-ownership gate, byte-identical to signal/cancel.
    let target = WorkflowTarget::workflow(&request.workflow_id);
    let operation = NamespaceOperation::intervene(&request.namespace, target);
    state.namespace_guard().scope(caller, &operation).await?;

    // Stamp the neutral command from the authenticated caller + receive instant so
    // the transcript attribution is server-owned, never client-forged. `issued_by`
    // is `None` when auth is off (an unauthenticated operator has no subject).
    let issued_by = match caller.subject() {
        "" => None,
        subject => Some(subject.to_owned()),
    };
    let command = InterventionCommand {
        workflow_id: request.workflow_id,
        activity_id: request.activity_id,
        attempt: request.attempt,
        issued_by,
        issued_at: chrono::Utc::now(),
        kind: request.kind,
    };
    state.intervention_router().route(command).await
}

#[cfg(test)]
mod tests {
    use super::{InterveneRequest, InterveneResponse};
    use aion_core::{
        ActivityId, InjectPriority, InterventionKind, InterventionOutcome, InterventionPrimitive,
        WorkflowId,
    };

    /// The request body carries the target identity + neutral primitive and
    /// round-trips through serde; `issued_by`/`issued_at` are NOT client fields.
    #[test]
    fn request_body_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let request = InterveneRequest {
            namespace: "tenant-a".to_owned(),
            workflow_id: WorkflowId::new(uuid::Uuid::nil()),
            activity_id: ActivityId::from_sequence_position(3),
            attempt: 1,
            kind: InterventionKind::InjectMessage {
                text: "steer".to_owned(),
                priority: InjectPriority::Interrupt,
            },
        };
        let json = serde_json::to_string(&request)?;
        let decoded: InterveneRequest = serde_json::from_str(&json)?;
        assert_eq!(decoded.namespace, "tenant-a");
        assert_eq!(decoded.attempt, 1);
        assert!(matches!(
            decoded.kind,
            InterventionKind::InjectMessage { .. }
        ));
        // A client cannot set issued_by/issued_at: they are not request fields.
        assert!(!json.contains("issued_by"));
        assert!(!json.contains("issued_at"));
        Ok(())
    }

    /// The response body carries the neutral ack (each of the three classes).
    #[test]
    fn response_body_round_trips_each_outcome() -> Result<(), Box<dyn std::error::Error>> {
        let outcomes = [
            InterventionOutcome::Applied,
            InterventionOutcome::capability_not_supported(InterventionPrimitive::PauseResume),
            InterventionOutcome::stale_target("attempt superseded"),
        ];
        for outcome in outcomes {
            let response = InterveneResponse {
                outcome: outcome.clone(),
            };
            let json = serde_json::to_string(&response)?;
            let decoded: InterveneResponse = serde_json::from_str(&json)?;
            assert_eq!(decoded.outcome, outcome);
        }
        Ok(())
    }
}
