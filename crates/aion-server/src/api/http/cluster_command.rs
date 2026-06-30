//! `/cluster/command` HTTP facade for the ADR-020 cluster command seam (WS3).
//!
//! Phase 1 ships exactly one *mutating* blast radius: zero. The only command
//! that does real work is the read-only [`ClusterCommand::RequestClusterSnapshot`]
//! (the ops console's calm-state baseline; also obtainable as the WS priming
//! reply). Every mutating variant compiles so the contract exists, but its
//! handler runs the full deploy-auth gate FIRST and then returns an
//! `unimplemented` wire error — so the seam's authorization contract is
//! exercised and tested now, and an `unimplemented` stub is never an
//! auth-bypass-shaped hole.
//!
//! Auth: `HttpCaller` (header-based; a browser CAN set headers on a POST, so no
//! query-param promotion is needed). The gate is the deployment-wide deploy
//! grant — cluster commands are deployment-scoped, never namespace-scoped.

use aion_core::ClusterCommand;
use aion_proto::WireError;
use axum::{
    Json,
    extract::State,
    response::{IntoResponse, Response},
};

use super::auth::HttpCaller;
use super::error::HttpWireError;
use crate::ServerState;
use crate::namespace::CallerIdentity;

/// Handle a cluster command. Deploy-gated first, then dispatched.
pub(crate) async fn cluster_command(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(command): Json<ClusterCommand>,
) -> Result<Response, HttpWireError> {
    // GATE FIRST for EVERY variant — including the aspirational ones — so the
    // auth contract is exercised before any branch returns, and an unimplemented
    // handler can never be an auth bypass.
    deploy_gate(&caller)?;

    match command {
        ClusterCommand::RequestClusterSnapshot {} => {
            let snapshot = crate::stream::cluster_stream::build_snapshot(&state, &caller)
                .map_err(|error| HttpWireError(error.to_wire_error()))?;
            Ok(Json(snapshot).into_response())
        }
        // Aspirational ADR-020 mutating commands: the gate already passed above,
        // so reaching here proves the deploy grant was checked; the handler then
        // declines with a typed unimplemented error and zero side effects.
        ClusterCommand::CancelWorkflow { .. }
        | ClusterCommand::ReopenWorkflow { .. }
        | ClusterCommand::RedriveOutboxRow { .. }
        | ClusterCommand::DrainNode { .. }
        | ClusterCommand::PlannedHandoff { .. }
        | ClusterCommand::ChaosKillNode { .. } => Err(HttpWireError(WireError::backend_with_type(
            "Unimplemented",
            "this cluster command is part of the ADR-020 seam but is not implemented in Phase 1",
        ))),
    }
}

/// Require the deployment-wide deploy grant. Denial is a `deploy_denied` wire
/// error (403), the same shape the deploy API uses.
fn deploy_gate(caller: &CallerIdentity) -> Result<(), HttpWireError> {
    if caller.deploy_granted() {
        Ok(())
    } else {
        Err(HttpWireError(WireError::deploy_denied(
            "cluster commands require the deployment-wide deploy grant",
        )))
    }
}
