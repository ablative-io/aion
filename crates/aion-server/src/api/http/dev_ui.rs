//! `/dev/*` HTTP facade over the transport-agnostic dev-server handlers.
//!
//! Mounted only when `[dev].enabled` is true; the absent surface is a plain 404
//! (see `router.rs`). Each route is a thin shell over a [`crate::dev_ui`]
//! handler that drives the real engine, store, and event stream:
//!
//! * `POST /dev/runs` triggers a run and returns the existing-firehose
//!   subscription for it;
//! * `POST /dev/mocks` installs an opt-in per-run activity mock;
//! * `POST /dev/replay` re-drives a failed run through the real engine.

use axum::{Json, extract::State};

use super::auth::HttpCaller;
use super::error::HttpWireError;
use crate::ServerState;
use crate::dev_ui::{
    RegisterMockRequest, RegisterMockResponse, ReplayRunRequest, ReplayRunResponse,
    TriggerRunRequest, TriggerRunResponse, register_mock, replay_run, trigger_run,
};

pub(crate) async fn dev_trigger_run(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<TriggerRunRequest>,
) -> Result<Json<TriggerRunResponse>, HttpWireError> {
    trigger_run(&state, &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

pub(crate) async fn dev_register_mock(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<RegisterMockRequest>,
) -> Result<Json<RegisterMockResponse>, HttpWireError> {
    register_mock(&state, &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

pub(crate) async fn dev_replay_run(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ReplayRunRequest>,
) -> Result<Json<ReplayRunResponse>, HttpWireError> {
    replay_run(&state, &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}
