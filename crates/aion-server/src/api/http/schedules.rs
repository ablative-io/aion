//! Schedule management handlers.

use aion_proto::{
    ProtoCreateScheduleRequest, ProtoCreateScheduleResponse, ProtoDeleteScheduleResponse,
    ProtoDescribeScheduleResponse, ProtoListSchedulesRequest, ProtoListSchedulesResponse,
    ProtoPauseScheduleResponse, ProtoResumeScheduleResponse, ProtoScheduleId,
    ProtoScheduleIdRequest, ProtoUpdateScheduleRequest, ProtoUpdateScheduleResponse,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;

use super::auth::HttpCaller;
use super::error::HttpWireError;
use crate::{ServerState, api::schedule_handlers};

#[derive(Deserialize)]
pub(crate) struct NamespaceQuery {
    namespace: String,
}

pub(crate) async fn create_schedule(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Json(request): Json<ProtoCreateScheduleRequest>,
) -> Result<(StatusCode, Json<ProtoCreateScheduleResponse>), HttpWireError> {
    schedule_handlers::create_schedule(state.namespace_guard(), &caller, request)
        .await
        .map(|response| (StatusCode::CREATED, Json(response)))
        .map_err(HttpWireError)
}

pub(crate) async fn update_schedule(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(id): Path<String>,
    Json(mut request): Json<ProtoUpdateScheduleRequest>,
) -> Result<Json<ProtoUpdateScheduleResponse>, HttpWireError> {
    request.schedule_id = Some(ProtoScheduleId { uuid: id });
    schedule_handlers::update_schedule(state.namespace_guard(), &caller, request)
        .await
        .map(Json)
        .map_err(HttpWireError)
}

pub(crate) async fn pause_schedule(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(id): Path<String>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<ProtoPauseScheduleResponse>, HttpWireError> {
    schedule_handlers::pause_schedule(
        state.namespace_guard(),
        &caller,
        schedule_id_request(query, id),
    )
    .await
    .map(Json)
    .map_err(HttpWireError)
}

pub(crate) async fn resume_schedule(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(id): Path<String>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<ProtoResumeScheduleResponse>, HttpWireError> {
    schedule_handlers::resume_schedule(
        state.namespace_guard(),
        &caller,
        schedule_id_request(query, id),
    )
    .await
    .map(Json)
    .map_err(HttpWireError)
}

pub(crate) async fn delete_schedule(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(id): Path<String>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<ProtoDeleteScheduleResponse>, HttpWireError> {
    schedule_handlers::delete_schedule(
        state.namespace_guard(),
        &caller,
        schedule_id_request(query, id),
    )
    .await
    .map(Json)
    .map_err(HttpWireError)
}

pub(crate) async fn list_schedules(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<ProtoListSchedulesResponse>, HttpWireError> {
    schedule_handlers::list_schedules(
        state.namespace_guard(),
        &caller,
        ProtoListSchedulesRequest {
            namespace: query.namespace,
        },
    )
    .await
    .map(Json)
    .map_err(HttpWireError)
}

pub(crate) async fn describe_schedule(
    State(state): State<ServerState>,
    HttpCaller(caller): HttpCaller,
    Path(id): Path<String>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<ProtoDescribeScheduleResponse>, HttpWireError> {
    schedule_handlers::describe_schedule(
        state.namespace_guard(),
        &caller,
        schedule_id_request(query, id),
    )
    .await
    .map(Json)
    .map_err(HttpWireError)
}

fn schedule_id_request(query: NamespaceQuery, id: String) -> ProtoScheduleIdRequest {
    ProtoScheduleIdRequest {
        namespace: query.namespace,
        schedule_id: Some(ProtoScheduleId { uuid: id }),
    }
}
