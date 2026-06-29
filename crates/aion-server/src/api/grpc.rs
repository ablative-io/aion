//! tonic workflow service adapter.

use aion_proto::{
    ProtoCancelRequest, ProtoCancelResponse, ProtoCountWorkflowsRequest,
    ProtoCountWorkflowsResponse, ProtoCreateScheduleRequest, ProtoCreateScheduleResponse,
    ProtoDeleteScheduleResponse, ProtoDescribeScheduleResponse, ProtoDescribeWorkflowRequest,
    ProtoDescribeWorkflowResponse, ProtoListSchedulesRequest, ProtoListSchedulesResponse,
    ProtoListWorkflowsRequest, ProtoListWorkflowsResponse, ProtoPauseScheduleResponse,
    ProtoQueryRequest, ProtoQueryResponse, ProtoResumeScheduleResponse, ProtoScheduleIdRequest,
    ProtoSignalRequest, ProtoSignalResponse, ProtoStartWorkflowRequest, ProtoStartWorkflowResponse,
    ProtoUpdateScheduleRequest, ProtoUpdateScheduleResponse, ProtoWireError, WireError,
    generated::{self, workflow_service_server::WorkflowServiceServer},
};
use prost::Message;
use tonic::{Code, Request, Response, Status};

use crate::{CallerIdentity, ServerState, api::handlers, api::schedule_handlers};

/// Cloneable tonic implementation for workflow management.
#[derive(Clone)]
pub struct WorkflowGrpcService {
    state: ServerState,
}

impl WorkflowGrpcService {
    /// Build a tonic workflow service from shared server state.
    #[must_use]
    pub const fn new(state: ServerState) -> Self {
        Self { state }
    }

    async fn caller<T>(&self, request: &Request<T>) -> Result<CallerIdentity, Status> {
        caller_from_metadata(request.metadata(), &self.state).await
    }

    /// Resolve routing for a signal/query/cancel at the edge (R-1/R-2/R-3).
    ///
    /// Returns:
    /// - [`RouteResolution::Local`] — proceed to the local engine handler. This
    ///   is the only outcome for single-node / non-clustered boots (no cluster
    ///   store), so the default path is unchanged.
    /// - [`RouteResolution::Reply`] — the request was forwarded to the owner and
    ///   this is its relayed reply.
    /// - [`RouteResolution::Reject`] — return this typed `NotOwner` status (no
    ///   forward target, hop cap exceeded, or re-resolution still off-owner).
    ///
    /// `workflow_id` is the request's (optional) proto id; a missing/malformed id
    /// is left to the handler's existing validation (routing only acts on a
    /// well-formed target). `metadata` is the inbound caller metadata — copied
    /// onto the forward so the owner authorizes identically — and carries the hop
    /// count for loop prevention. `request` is the verbatim RPC to relay.
    #[cfg(feature = "haematite-backend")]
    async fn resolve_route(
        &self,
        workflow_id: Option<aion_proto::ProtoWorkflowId>,
        metadata: &tonic::metadata::MetadataMap,
        request: crate::routing::ForwardRequest,
    ) -> RouteResolution {
        use crate::routing::{RouteDecision, route_mutation};
        let Some(cluster_store) = self.state.cluster_store() else {
            return RouteResolution::Local;
        };
        let Some(proto) = workflow_id else {
            return RouteResolution::Local;
        };
        let Ok(workflow_id) = aion_core::WorkflowId::try_from(proto) else {
            return RouteResolution::Local;
        };
        let directory = self
            .state
            .shard_directory()
            .map(|directory| directory.as_ref() as &dyn crate::routing::ShardDirectory);
        match route_mutation(Some(cluster_store.as_ref()), directory, &workflow_id) {
            RouteDecision::Local => RouteResolution::Local,
            RouteDecision::NotOwner { shard } => RouteResolution::Reject(not_owner_status(shard)),
            RouteDecision::Forward { owner, shard } => {
                self.forward_or_reject(owner, shard, metadata, request)
                    .await
            }
        }
    }

    /// Forward a resolved non-local request to `owner`, enforcing the hop cap and
    /// returning a typed `NotOwner` rather than forwarding when the owner is not
    /// forwardable, the cap is exceeded, or the forward itself reports the target
    /// is stale (§2.5: re-resolve-or-reject discipline).
    #[cfg(feature = "haematite-backend")]
    async fn forward_or_reject(
        &self,
        owner: crate::routing::NodeRef,
        shard: usize,
        metadata: &tonic::metadata::MetadataMap,
        request: crate::routing::ForwardRequest,
    ) -> RouteResolution {
        use crate::routing::{MAX_FORWARD_HOPS, current_hops};
        // Loop prevention: a request that has already taken the maximum number of
        // hops is not forwarded again — break the chain with NotOwner so the
        // original caller re-resolves with backoff.
        if current_hops(metadata) >= MAX_FORWARD_HOPS {
            return RouteResolution::Reject(not_owner_status(shard));
        }
        // A known-but-not-forwardable owner (no declared gRPC address) → NotOwner.
        let Some(target) = owner.grpc_addr else {
            return RouteResolution::Reject(not_owner_status(shard));
        };
        let Some(forwarder) = self.state.request_forwarder() else {
            return RouteResolution::Reject(not_owner_status(shard));
        };
        match forwarder.forward(target, metadata.clone(), request).await {
            Ok(reply) => RouteResolution::Reply(reply),
            // The forward target is stale or unreachable (it may have just died /
            // not yet adopted). Return NotOwner so the caller re-resolves; under
            // the v1.5 overlay the directory then sees the target down (§2.5).
            Err(_status) => RouteResolution::Reject(not_owner_status(shard)),
        }
    }

    /// R-1 unsteered-start placement: an id re-minted onto a locally-owned shard
    /// when this clustered node owns only a subset of shards, else `None` (engine
    /// mints as usual — the default path).
    #[cfg(feature = "haematite-backend")]
    fn start_placement(&self) -> Option<aion_core::WorkflowId> {
        use crate::routing::{RemintOutcome, route_start};
        match route_start(self.state.cluster_store().map(AsRef::as_ref)) {
            RemintOutcome::UseId(workflow_id) => Some(workflow_id),
            RemintOutcome::EngineMint => None,
        }
    }

    /// Resolve a `start` at the edge (R-4 steered start over R-1 remint).
    ///
    /// With a non-empty `routing_key` on a clustered node, the target shard is
    /// derived from the key and the start is steered to its owner: forwarded when
    /// a live remote node owns the shard, run locally on a key-shard-minted id
    /// otherwise, or rejected `NotOwner` when the owner is unreachable. With no
    /// routing key (or no cluster store) this falls back to the R-1 unsteered
    /// remint — so the single-node / unsteered path is unchanged.
    #[cfg(feature = "haematite-backend")]
    async fn resolve_start(
        &self,
        request: &generated::StartWorkflowRequest,
        metadata: &tonic::metadata::MetadataMap,
    ) -> StartResolution {
        use crate::routing::{SteerDecision, route_start_steered};
        let routing_key = request.routing_key.as_deref().filter(|key| !key.is_empty());
        let Some(routing_key) = routing_key else {
            // Unsteered: keep the R-1 remint behaviour exactly.
            return StartResolution::Local(self.start_placement());
        };
        let Some(cluster_store) = self.state.cluster_store() else {
            // A routing key on a non-clustered node has no shards to steer to:
            // let the engine mint as usual (unsteered fallback).
            return StartResolution::Local(None);
        };
        let directory = self
            .state
            .shard_directory()
            .map(|directory| directory.as_ref() as &dyn crate::routing::ShardDirectory);
        match route_start_steered(cluster_store.as_ref(), directory, routing_key) {
            SteerDecision::Local(workflow_id) => StartResolution::Local(Some(workflow_id)),
            SteerDecision::NotOwner { shard } => StartResolution::Reject(not_owner_status(shard)),
            SteerDecision::Forward { owner, shard } => {
                self.forward_or_reject_start(owner, shard, metadata, request.clone())
                    .await
            }
        }
    }

    /// Forward a steered `start` to its shard owner, enforcing the hop cap and
    /// returning `NotOwner` rather than forwarding when the owner is not
    /// forwardable, the cap is exceeded, or the forward reports a stale target
    /// (§2.5 re-resolve-or-reject — identical discipline to signal/query/cancel).
    #[cfg(feature = "haematite-backend")]
    async fn forward_or_reject_start(
        &self,
        owner: crate::routing::NodeRef,
        shard: usize,
        metadata: &tonic::metadata::MetadataMap,
        request: generated::StartWorkflowRequest,
    ) -> StartResolution {
        use crate::routing::{ForwardReply, ForwardRequest, MAX_FORWARD_HOPS, current_hops};
        if current_hops(metadata) >= MAX_FORWARD_HOPS {
            return StartResolution::Reject(not_owner_status(shard));
        }
        let Some(target) = owner.grpc_addr else {
            return StartResolution::Reject(not_owner_status(shard));
        };
        let Some(forwarder) = self.state.request_forwarder() else {
            return StartResolution::Reject(not_owner_status(shard));
        };
        match forwarder
            .forward(target, metadata.clone(), ForwardRequest::Start(request))
            .await
        {
            Ok(ForwardReply::Start(reply)) => StartResolution::Reply(reply),
            Ok(_) => {
                StartResolution::Reject(Status::internal("forwarder returned a mismatched reply"))
            }
            // Stale/unreachable target: NotOwner so the caller re-resolves (§2.5).
            Err(_status) => StartResolution::Reject(not_owner_status(shard)),
        }
    }
}

/// The edge's routing outcome for a `start` (R-1 remint / R-4 steered start).
#[cfg(feature = "haematite-backend")]
enum StartResolution {
    /// Run the start locally with this placement id (`None` → engine mints).
    Local(Option<aion_core::WorkflowId>),
    /// The steered start was forwarded; relay this reply to the caller.
    Reply(generated::StartWorkflowResponse),
    /// Return this typed status (`NotOwner` / internal) to the caller.
    Reject(Status),
}

/// The edge's routing outcome for a signal/query/cancel (R-3).
#[cfg(feature = "haematite-backend")]
enum RouteResolution {
    /// Proceed to the local engine handler.
    Local,
    /// The request was forwarded; relay this reply to the caller.
    Reply(crate::routing::ForwardReply),
    /// Return this typed `NotOwner` status to the caller.
    Reject(Status),
}

/// Build the typed retryable `NotOwner` tonic status for shard `shard` (R-1).
#[cfg(feature = "haematite-backend")]
fn not_owner_status(shard: usize) -> Status {
    let wire = WireError::not_owner(format!(
        "workflow shard {shard} is owned by another cluster node"
    ))
    .with_error_type("NotOwner");
    status_from_wire_error(wire)
}

/// Construct the generated tonic server wrapper.
#[must_use]
pub fn workflow_service(state: ServerState) -> WorkflowServiceServer<WorkflowGrpcService> {
    WorkflowServiceServer::new(WorkflowGrpcService::new(state))
}

#[tonic::async_trait]
impl generated::workflow_service_server::WorkflowService for WorkflowGrpcService {
    async fn start_workflow(
        &self,
        request: Request<generated::StartWorkflowRequest>,
    ) -> Result<Response<generated::StartWorkflowResponse>, Status> {
        if self.state.drain_state().is_draining() {
            return Err(Status::unavailable(
                "server is draining and not accepting new workflow starts",
            ));
        }
        let caller = self.caller(&request).await?;
        // R-4 steered start over R-1 remint: a non-empty routing key steers the
        // start to its shard owner (forwarding when remote); otherwise the R-1
        // remint places it locally. `placement` is `None` for single-node /
        // non-clustered boots (and own-all scope), so the engine mints as usual —
        // default path unchanged. Without the cluster backend there is no
        // steering or placement at all.
        #[cfg(feature = "haematite-backend")]
        {
            let (metadata, _ext, inner) = request.into_parts();
            let placement = match self.resolve_start(&inner, &metadata).await {
                StartResolution::Reject(status) => return Err(status),
                StartResolution::Reply(reply) => return Ok(Response::new(reply)),
                StartResolution::Local(placement) => placement,
            };
            let response = handlers::start_with_placement(
                self.state.namespace_guard(),
                &caller,
                decode_start_request(inner),
                placement,
            )
            .await
            .map_err(status_from_wire_error)?;
            return Ok(Response::new(encode_start_response(response)));
        }
        #[cfg(not(feature = "haematite-backend"))]
        {
            let placement: Option<aion_core::WorkflowId> = None;
            let response = handlers::start_with_placement(
                self.state.namespace_guard(),
                &caller,
                decode_start_request(request.into_inner()),
                placement,
            )
            .await
            .map_err(status_from_wire_error)?;
            Ok(Response::new(encode_start_response(response)))
        }
    }

    async fn signal(
        &self,
        request: Request<generated::SignalRequest>,
    ) -> Result<Response<generated::SignalResponse>, Status> {
        let caller = self.caller(&request).await?;
        #[cfg(feature = "haematite-backend")]
        {
            use crate::routing::{ForwardReply, ForwardRequest};
            let (metadata, _ext, inner) = request.into_parts();
            let workflow_id = inner.workflow_id.clone().map(decode_workflow_id);
            match self
                .resolve_route(
                    workflow_id,
                    &metadata,
                    ForwardRequest::Signal(inner.clone()),
                )
                .await
            {
                RouteResolution::Reject(status) => return Err(status),
                RouteResolution::Reply(ForwardReply::Signal(reply)) => {
                    return Ok(Response::new(reply));
                }
                RouteResolution::Reply(_) => {
                    return Err(Status::internal("forwarder returned a mismatched reply"));
                }
                RouteResolution::Local => {
                    let response = handlers::signal(
                        self.state.namespace_guard(),
                        &caller,
                        decode_signal_request(inner),
                    )
                    .await
                    .map_err(status_from_wire_error)?;
                    return Ok(Response::new(encode_signal_response(response)));
                }
            }
        }
        #[cfg(not(feature = "haematite-backend"))]
        {
            let response = handlers::signal(
                self.state.namespace_guard(),
                &caller,
                decode_signal_request(request.into_inner()),
            )
            .await
            .map_err(status_from_wire_error)?;
            Ok(Response::new(encode_signal_response(response)))
        }
    }

    async fn query(
        &self,
        request: Request<generated::QueryRequest>,
    ) -> Result<Response<generated::QueryResponse>, Status> {
        let caller = self.caller(&request).await?;
        #[cfg(feature = "haematite-backend")]
        {
            use crate::routing::{ForwardReply, ForwardRequest};
            let (metadata, _ext, inner) = request.into_parts();
            let workflow_id = inner.workflow_id.clone().map(decode_workflow_id);
            match self
                .resolve_route(workflow_id, &metadata, ForwardRequest::Query(inner.clone()))
                .await
            {
                RouteResolution::Reject(status) => return Err(status),
                RouteResolution::Reply(ForwardReply::Query(reply)) => {
                    return Ok(Response::new(reply));
                }
                RouteResolution::Reply(_) => {
                    return Err(Status::internal("forwarder returned a mismatched reply"));
                }
                RouteResolution::Local => {
                    let response = handlers::query(
                        self.state.namespace_guard(),
                        &caller,
                        decode_query_request(inner),
                    )
                    .await
                    .map_err(status_from_wire_error)?;
                    return Ok(Response::new(encode_query_response(response)));
                }
            }
        }
        #[cfg(not(feature = "haematite-backend"))]
        {
            let response = handlers::query(
                self.state.namespace_guard(),
                &caller,
                decode_query_request(request.into_inner()),
            )
            .await
            .map_err(status_from_wire_error)?;
            Ok(Response::new(encode_query_response(response)))
        }
    }

    async fn cancel(
        &self,
        request: Request<generated::CancelRequest>,
    ) -> Result<Response<generated::CancelResponse>, Status> {
        let caller = self.caller(&request).await?;
        #[cfg(feature = "haematite-backend")]
        {
            use crate::routing::{ForwardReply, ForwardRequest};
            let (metadata, _ext, inner) = request.into_parts();
            let workflow_id = inner.workflow_id.clone().map(decode_workflow_id);
            match self
                .resolve_route(
                    workflow_id,
                    &metadata,
                    ForwardRequest::Cancel(inner.clone()),
                )
                .await
            {
                RouteResolution::Reject(status) => return Err(status),
                RouteResolution::Reply(ForwardReply::Cancel(reply)) => {
                    return Ok(Response::new(reply));
                }
                RouteResolution::Reply(_) => {
                    return Err(Status::internal("forwarder returned a mismatched reply"));
                }
                RouteResolution::Local => {
                    let response = handlers::cancel(
                        self.state.namespace_guard(),
                        &caller,
                        decode_cancel_request(inner),
                    )
                    .await
                    .map_err(status_from_wire_error)?;
                    return Ok(Response::new(encode_cancel_response(response)));
                }
            }
        }
        #[cfg(not(feature = "haematite-backend"))]
        {
            let response = handlers::cancel(
                self.state.namespace_guard(),
                &caller,
                decode_cancel_request(request.into_inner()),
            )
            .await
            .map_err(status_from_wire_error)?;
            Ok(Response::new(encode_cancel_response(response)))
        }
    }

    async fn list_workflows(
        &self,
        request: Request<generated::ListWorkflowsRequest>,
    ) -> Result<Response<generated::ListWorkflowsResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::list(
            self.state.namespace_guard(),
            &caller,
            decode_list_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_list_response(response)))
    }

    async fn count_workflows(
        &self,
        request: Request<generated::CountWorkflowsRequest>,
    ) -> Result<Response<generated::CountWorkflowsResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::count(
            self.state.namespace_guard(),
            &caller,
            decode_count_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_count_response(response)))
    }

    async fn describe_workflow(
        &self,
        request: Request<generated::DescribeWorkflowRequest>,
    ) -> Result<Response<generated::DescribeWorkflowResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = handlers::describe(
            self.state.namespace_guard(),
            &caller,
            decode_describe_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_describe_response(response)))
    }

    async fn create_schedule(
        &self,
        request: Request<generated::CreateScheduleRequest>,
    ) -> Result<Response<generated::CreateScheduleResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = schedule_handlers::create_schedule(
            self.state.namespace_guard(),
            &caller,
            decode_create_schedule_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_create_schedule_response(response)))
    }

    async fn update_schedule(
        &self,
        request: Request<generated::UpdateScheduleRequest>,
    ) -> Result<Response<generated::UpdateScheduleResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = schedule_handlers::update_schedule(
            self.state.namespace_guard(),
            &caller,
            decode_update_schedule_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_update_schedule_response(response)))
    }

    async fn pause_schedule(
        &self,
        request: Request<generated::ScheduleIdRequest>,
    ) -> Result<Response<generated::PauseScheduleResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = schedule_handlers::pause_schedule(
            self.state.namespace_guard(),
            &caller,
            decode_schedule_id_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_pause_schedule_response(response)))
    }

    async fn resume_schedule(
        &self,
        request: Request<generated::ScheduleIdRequest>,
    ) -> Result<Response<generated::ResumeScheduleResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = schedule_handlers::resume_schedule(
            self.state.namespace_guard(),
            &caller,
            decode_schedule_id_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_resume_schedule_response(response)))
    }

    async fn delete_schedule(
        &self,
        request: Request<generated::ScheduleIdRequest>,
    ) -> Result<Response<generated::DeleteScheduleResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = schedule_handlers::delete_schedule(
            self.state.namespace_guard(),
            &caller,
            decode_schedule_id_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_delete_schedule_response(response)))
    }

    async fn list_schedules(
        &self,
        request: Request<generated::ListSchedulesRequest>,
    ) -> Result<Response<generated::ListSchedulesResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = schedule_handlers::list_schedules(
            self.state.namespace_guard(),
            &caller,
            decode_list_schedules_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_list_schedules_response(response)))
    }

    async fn describe_schedule(
        &self,
        request: Request<generated::ScheduleIdRequest>,
    ) -> Result<Response<generated::DescribeScheduleResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = schedule_handlers::describe_schedule(
            self.state.namespace_guard(),
            &caller,
            decode_schedule_id_request(request.into_inner()),
        )
        .await
        .map_err(status_from_wire_error)?;
        Ok(Response::new(encode_describe_schedule_response(response)))
    }
}

pub(crate) async fn caller_from_metadata(
    metadata: &tonic::metadata::MetadataMap,
    state: &ServerState,
) -> Result<CallerIdentity, Status> {
    if !state.runtime_config().auth.enabled {
        return Ok(development_caller_from_metadata(metadata));
    }
    #[cfg(feature = "auth")]
    {
        let bearer = metadata
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(parse_bearer)
            .ok_or_else(|| Status::unauthenticated("missing bearer token"))?;
        let Some(cache) = state.jwks_cache() else {
            return Err(Status::unauthenticated("invalid bearer token"));
        };
        return cache
            .validate(&bearer)
            .await
            .map(|claims| claims.caller_identity())
            .map_err(|_error| Status::unauthenticated("invalid bearer token"));
    }
    #[cfg(not(feature = "auth"))]
    {
        // Yield to preserve the async signature required by the auth-feature branch.
        tokio::task::yield_now().await;
        Ok(development_token_caller_from_metadata(
            metadata,
            &state.runtime_config().auth,
        ))
    }
}

fn development_caller_from_metadata(metadata: &tonic::metadata::MetadataMap) -> CallerIdentity {
    let subject = metadata
        .get("x-aion-subject")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or("anonymous");
    let namespaces = metadata
        .get("x-aion-namespaces")
        .and_then(|value| value.to_str().ok())
        .map(parse_namespaces)
        .unwrap_or_default();
    CallerIdentity::new(subject, namespaces).with_deploy(deploy_metadata_granted(metadata))
}

/// Deployment-wide deploy grant from the development `x-aion-deploy`
/// metadata entry, the dev-mode analog of the JWT `deploy` claim.
fn deploy_metadata_granted(metadata: &tonic::metadata::MetadataMap) -> bool {
    metadata
        .get("x-aion-deploy")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

/// Development-mode token authentication for gRPC metadata, mirroring the HTTP
/// development token auth.  Used when `auth.enabled` is `true` but the `auth`
/// crate feature is not compiled.
#[cfg(not(feature = "auth"))]
fn development_token_caller_from_metadata(
    metadata: &tonic::metadata::MetadataMap,
    auth: &crate::config::AuthConfig,
) -> CallerIdentity {
    let subject = metadata
        .get("x-aion-subject")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty());
    let namespaces = metadata
        .get("x-aion-namespaces")
        .and_then(|value| value.to_str().ok())
        .map(parse_namespaces)
        .unwrap_or_default();

    let bearer_token = auth.jwks_url.as_deref().unwrap_or_default();
    let expected = format!("Bearer {bearer_token}");
    let Some(authorization) = metadata.get("authorization") else {
        return CallerIdentity::denied(subject.unwrap_or("anonymous"), "missing bearer token");
    };
    let authorization = authorization.to_str().ok();
    if authorization != Some(expected.as_str()) {
        return CallerIdentity::denied(subject.unwrap_or("anonymous"), "invalid bearer token");
    }

    let Some(subject) = subject else {
        return CallerIdentity::denied("anonymous", "missing required metadata: x-aion-subject");
    };

    CallerIdentity::new(subject, namespaces).with_deploy(deploy_metadata_granted(metadata))
}

#[cfg(feature = "auth")]
fn parse_bearer(value: &str) -> Option<String> {
    let token = value.strip_prefix("Bearer ")?.trim();
    if token.is_empty() {
        return None;
    }
    Some(token.to_owned())
}

fn parse_namespaces(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|namespace| !namespace.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(crate) fn status_from_wire_error(error: WireError) -> Status {
    status_with_code(grpc_code(error.code), error)
}

/// Build a tonic status with an explicit code, carrying the typed
/// `ProtoWireError` detail payload when it encodes.
pub(crate) fn status_with_code(code: Code, error: WireError) -> Status {
    let message = error.message.clone();
    let mut details = Vec::new();
    let proto_error = ProtoWireError::from(error);
    if proto_error.encode(&mut details).is_ok() {
        Status::with_details(code, message, details.into())
    } else {
        Status::new(code, message)
    }
}

fn grpc_code(code: aion_proto::WireErrorCode) -> Code {
    match code {
        aion_proto::WireErrorCode::NotFound => Code::NotFound,
        aion_proto::WireErrorCode::NamespaceDenied | aion_proto::WireErrorCode::DeployDenied => {
            Code::PermissionDenied
        }
        // Wrong-shard-owner (fenced) is a retryable routing signal: surface it as
        // `Aborted`, the same retryable code the CAS `SequenceConflict` precedent
        // uses (R-0). A routing-aware caller re-resolves the owner and retries.
        aion_proto::WireErrorCode::SequenceConflict | aion_proto::WireErrorCode::NotOwner => {
            Code::Aborted
        }
        aion_proto::WireErrorCode::UnknownQuery | aion_proto::WireErrorCode::InvalidInput => {
            Code::InvalidArgument
        }
        aion_proto::WireErrorCode::QueryTimeout => Code::DeadlineExceeded,
        aion_proto::WireErrorCode::NotRunning | aion_proto::WireErrorCode::VersionPinned => {
            Code::FailedPrecondition
        }
        aion_proto::WireErrorCode::Lagged => Code::ResourceExhausted,
        // query_failed normally rides QueryResponse.error inside an OK
        // response; a transport-level carrier still attaches the typed
        // ProtoWireError detail, so detail-aware clients keep QueryFailed.
        aion_proto::WireErrorCode::Backend | aion_proto::WireErrorCode::QueryFailed => {
            Code::Internal
        }
    }
}

fn decode_workflow_id(value: generated::WorkflowId) -> aion_proto::ProtoWorkflowId {
    aion_proto::ProtoWorkflowId { uuid: value.uuid }
}

fn encode_workflow_id(value: aion_proto::ProtoWorkflowId) -> generated::WorkflowId {
    generated::WorkflowId { uuid: value.uuid }
}

fn decode_run_id(value: generated::RunId) -> aion_proto::ProtoRunId {
    aion_proto::ProtoRunId { uuid: value.uuid }
}

fn encode_run_id(value: aion_proto::ProtoRunId) -> generated::RunId {
    generated::RunId { uuid: value.uuid }
}

fn decode_schedule_id(value: generated::ScheduleId) -> aion_proto::ProtoScheduleId {
    aion_proto::ProtoScheduleId { uuid: value.uuid }
}

fn encode_schedule_id(value: aion_proto::ProtoScheduleId) -> generated::ScheduleId {
    generated::ScheduleId { uuid: value.uuid }
}

fn decode_payload(value: generated::Payload) -> aion_proto::ProtoPayload {
    aion_proto::ProtoPayload {
        content_type: value.content_type,
        bytes: value.bytes,
    }
}

fn encode_payload(value: aion_proto::ProtoPayload) -> generated::Payload {
    generated::Payload {
        content_type: value.content_type,
        bytes: value.bytes,
    }
}

fn decode_envelope(value: generated::WireEnvelope) -> aion_proto::WireEnvelope {
    aion_proto::WireEnvelope {
        namespace: value.namespace,
        request_id: value.request_id,
        payload: value.payload.map(decode_payload),
    }
}

fn encode_envelope(value: aion_proto::WireEnvelope) -> generated::WireEnvelope {
    generated::WireEnvelope {
        namespace: value.namespace,
        request_id: value.request_id,
        payload: value.payload.map(encode_payload),
    }
}

fn decode_start_request(value: generated::StartWorkflowRequest) -> ProtoStartWorkflowRequest {
    ProtoStartWorkflowRequest {
        namespace: value.namespace,
        workflow_type: value.workflow_type,
        input: value.input.map(decode_payload),
        routing_key: value.routing_key,
    }
}

fn encode_start_response(value: ProtoStartWorkflowResponse) -> generated::StartWorkflowResponse {
    generated::StartWorkflowResponse {
        workflow_id: value.workflow_id.map(encode_workflow_id),
        run_id: value.run_id.map(encode_run_id),
    }
}

fn decode_signal_request(value: generated::SignalRequest) -> ProtoSignalRequest {
    ProtoSignalRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(decode_workflow_id),
        run_id: value.run_id.map(decode_run_id),
        signal_name: value.signal_name,
        payload: value.payload.map(decode_payload),
    }
}

fn encode_signal_response(_: ProtoSignalResponse) -> generated::SignalResponse {
    generated::SignalResponse {}
}

fn decode_query_request(value: generated::QueryRequest) -> ProtoQueryRequest {
    ProtoQueryRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(decode_workflow_id),
        run_id: value.run_id.map(decode_run_id),
        query_name: value.query_name,
    }
}

fn encode_query_response(value: ProtoQueryResponse) -> generated::QueryResponse {
    generated::QueryResponse {
        outcome: value.outcome.map(encode_query_outcome),
    }
}

fn encode_query_outcome(
    value: aion_proto::proto_query_response::Outcome,
) -> generated::query_response::Outcome {
    match value {
        aion_proto::proto_query_response::Outcome::Result(payload) => {
            generated::query_response::Outcome::Result(encode_payload(payload))
        }
        aion_proto::proto_query_response::Outcome::Error(error) => {
            generated::query_response::Outcome::Error(encode_wire_error(error))
        }
    }
}

fn encode_wire_error(value: ProtoWireError) -> generated::WireError {
    generated::WireError {
        code: value.code,
        message: value.message,
        error_type: value.error_type,
    }
}

fn decode_cancel_request(value: generated::CancelRequest) -> ProtoCancelRequest {
    ProtoCancelRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(decode_workflow_id),
        run_id: value.run_id.map(decode_run_id),
        reason: value.reason,
    }
}

fn encode_cancel_response(_: ProtoCancelResponse) -> generated::CancelResponse {
    generated::CancelResponse {}
}

fn decode_list_request(value: generated::ListWorkflowsRequest) -> ProtoListWorkflowsRequest {
    ProtoListWorkflowsRequest {
        namespace: value.namespace,
        filter: value.filter.map(decode_envelope),
    }
}

fn encode_list_response(value: ProtoListWorkflowsResponse) -> generated::ListWorkflowsResponse {
    generated::ListWorkflowsResponse {
        summaries: value.summaries.into_iter().map(encode_envelope).collect(),
    }
}

fn decode_count_request(value: generated::CountWorkflowsRequest) -> ProtoCountWorkflowsRequest {
    ProtoCountWorkflowsRequest {
        namespace: value.namespace,
        filter: value.filter.map(decode_envelope),
    }
}

fn encode_count_response(value: ProtoCountWorkflowsResponse) -> generated::CountWorkflowsResponse {
    generated::CountWorkflowsResponse { count: value.count }
}

fn decode_describe_request(
    value: generated::DescribeWorkflowRequest,
) -> ProtoDescribeWorkflowRequest {
    ProtoDescribeWorkflowRequest {
        namespace: value.namespace,
        workflow_id: value.workflow_id.map(decode_workflow_id),
        run_id: value.run_id.map(decode_run_id),
        include_history: value.include_history,
    }
}

fn encode_describe_response(
    value: ProtoDescribeWorkflowResponse,
) -> generated::DescribeWorkflowResponse {
    generated::DescribeWorkflowResponse {
        summary: value.summary.map(encode_envelope),
        history: value.history.into_iter().map(encode_envelope).collect(),
    }
}

fn decode_create_schedule_request(
    value: generated::CreateScheduleRequest,
) -> ProtoCreateScheduleRequest {
    ProtoCreateScheduleRequest {
        namespace: value.namespace,
        config: value.config.map(decode_envelope),
    }
}

fn encode_create_schedule_response(
    value: ProtoCreateScheduleResponse,
) -> generated::CreateScheduleResponse {
    generated::CreateScheduleResponse {
        schedule_id: value.schedule_id.map(encode_schedule_id),
        state: value.state.map(encode_envelope),
    }
}

fn decode_update_schedule_request(
    value: generated::UpdateScheduleRequest,
) -> ProtoUpdateScheduleRequest {
    ProtoUpdateScheduleRequest {
        namespace: value.namespace,
        schedule_id: value.schedule_id.map(decode_schedule_id),
        config: value.config.map(decode_envelope),
    }
}

fn encode_update_schedule_response(
    value: ProtoUpdateScheduleResponse,
) -> generated::UpdateScheduleResponse {
    generated::UpdateScheduleResponse {
        state: value.state.map(encode_envelope),
    }
}

fn decode_schedule_id_request(value: generated::ScheduleIdRequest) -> ProtoScheduleIdRequest {
    ProtoScheduleIdRequest {
        namespace: value.namespace,
        schedule_id: value.schedule_id.map(decode_schedule_id),
    }
}

fn encode_pause_schedule_response(
    value: ProtoPauseScheduleResponse,
) -> generated::PauseScheduleResponse {
    generated::PauseScheduleResponse {
        state: value.state.map(encode_envelope),
    }
}

fn encode_resume_schedule_response(
    value: ProtoResumeScheduleResponse,
) -> generated::ResumeScheduleResponse {
    generated::ResumeScheduleResponse {
        state: value.state.map(encode_envelope),
    }
}

fn encode_delete_schedule_response(
    _: ProtoDeleteScheduleResponse,
) -> generated::DeleteScheduleResponse {
    generated::DeleteScheduleResponse {}
}

fn decode_list_schedules_request(
    value: generated::ListSchedulesRequest,
) -> ProtoListSchedulesRequest {
    ProtoListSchedulesRequest {
        namespace: value.namespace,
    }
}

fn encode_list_schedules_response(
    value: ProtoListSchedulesResponse,
) -> generated::ListSchedulesResponse {
    generated::ListSchedulesResponse {
        schedules: value.schedules.into_iter().map(encode_envelope).collect(),
    }
}

fn encode_describe_schedule_response(
    value: ProtoDescribeScheduleResponse,
) -> generated::DescribeScheduleResponse {
    generated::DescribeScheduleResponse {
        state: value.state.map(encode_envelope),
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::Arc};

    use aion::EngineBuilder;
    use aion_core::{Event, EventEnvelope, Payload, WorkflowId, WorkflowStatus};
    use aion_proto::{
        WireErrorCode,
        convert::{decode_core_value, encode_core_value},
        generated::workflow_service_server::WorkflowService,
    };
    use aion_store::{
        EventStore, InMemoryStore, WriteToken,
        visibility::{VisibilityRecord, VisibilityStore},
    };
    use chrono::Utc;
    use serde_json::json;
    use tonic::Request;

    use super::*;
    use crate::{
        NamespaceResolver,
        config::{
            AuthConfig, AuthoringConfig, DashboardAssetSource, DashboardConfig, DeployConfig,
            ListenConfig, MetricsConfig, NamespaceConfig, NamespaceMode, RuntimeConfig,
            WebSocketConfig, WorkerConfig,
        },
    };

    const NAMESPACE: &str = "tenant-a";
    const TOKEN: &str = "test-token";

    /// Server state whose bearer validation matches the compiled auth path:
    /// under `feature = "auth"` a real [`crate::auth::JwksCache`] is fetched
    /// from a live fixture JWKS endpoint; otherwise the development token path
    /// needs no cache.
    async fn server_state(
        resolver: NamespaceResolver,
        runtime: RuntimeConfig,
    ) -> Result<ServerState, Box<dyn std::error::Error>> {
        #[cfg(feature = "auth")]
        {
            let url = crate::auth::test_support::serve_jwks()?;
            let refresh = std::time::Duration::from_secs(runtime.auth.jwks_refresh_seconds);
            let cache = crate::auth::JwksCache::new(url, refresh).await?;
            Ok(ServerState::from_parts_with_jwks(resolver, runtime, cache))
        }
        #[cfg(not(feature = "auth"))]
        {
            // Yield to preserve the async signature required by the auth-feature branch.
            tokio::task::yield_now().await;
            Ok(ServerState::from_parts(resolver, runtime))
        }
    }

    /// R-0: the typed wrong-shard-owner fence maps to the retryable `Aborted`
    /// gRPC code (the same code the CAS `SequenceConflict` precedent uses), not
    /// the opaque `Internal` the stringly-typed fence used to collapse into.
    #[test]
    fn not_owner_wire_code_maps_to_retryable_aborted() {
        assert_eq!(grpc_code(WireErrorCode::NotOwner), Code::Aborted);
    }

    #[tokio::test]
    async fn in_process_tonic_start_and_list_use_shared_handlers()
    -> Result<(), Box<dyn std::error::Error>> {
        let backing = Arc::new(InMemoryStore::default());
        let store: Arc<dyn EventStore> = backing.clone();
        let visibility_store: Arc<dyn VisibilityStore> = backing;
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(Arc::clone(&store))
                .visibility_store_arc(Arc::clone(&visibility_store))
                .scheduler_threads(1)
                .build()
                .await?,
        );
        store
            .append(
                WriteToken::recorder(),
                &workflow_id(),
                &[started_event()?],
                0,
            )
            .await?;
        visibility_store
            .record_visibility(VisibilityRecord {
                workflow_id: workflow_id(),
                run_id: aion_core::RunId::new(uuid::Uuid::from_u128(2)),
                workflow_type: String::from("fixture"),
                status: WorkflowStatus::Running,
                start_time: Utc::now(),
                close_time: None,
                search_attributes: std::collections::HashMap::from([(
                    crate::namespace::NAMESPACE_ATTRIBUTE.to_owned(),
                    aion_core::SearchAttributeValue::String(NAMESPACE.to_owned()),
                )]),
            })
            .await?;
        let resolver = NamespaceResolver::from_config(
            crate::config::NamespaceConfig {
                mode: NamespaceMode::SharedEngine,
            },
            engine,
        );
        let state = server_state(resolver.clone(), runtime_config()).await?;
        let service = WorkflowGrpcService::new(state);

        let mut start = Request::new(generated::StartWorkflowRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_type: "missing-workflow".to_owned(),
            input: Some(encode_payload(proto_payload()?)),
            routing_key: None,
        });
        apply_metadata(start.metadata_mut())?;
        let start_error = service.start_workflow(start).await;
        let status = start_error
            .err()
            .ok_or_else(|| WireError::backend("expected error"))?;
        assert_eq!(status.code(), Code::NotFound);
        let detail = ProtoWireError::decode(status.details())?;
        assert_eq!(detail.error_type.as_deref(), Some("WorkflowTypeNotFound"));
        assert!(detail.message.contains("missing-workflow"));

        let list_filter = encode_core_value(
            NAMESPACE,
            None,
            &aion_store::visibility::ListWorkflowsFilter {
                workflow_type: Some(String::from("fixture")),
                status: Some(WorkflowStatus::Running),
                ..aion_store::visibility::ListWorkflowsFilter::default()
            },
        )?;
        let mut list = Request::new(generated::ListWorkflowsRequest {
            namespace: NAMESPACE.to_owned(),
            filter: Some(encode_envelope(list_filter)),
        });
        apply_metadata(list.metadata_mut())?;
        let response = service.list_workflows(list).await?.into_inner();

        assert_eq!(response.summaries.len(), 1);
        let summary = response
            .summaries
            .into_iter()
            .next()
            .map(decode_envelope)
            .map(|envelope| decode_core_value::<aion_store::visibility::WorkflowSummary>(&envelope))
            .transpose()?
            .ok_or_else(|| WireError::backend("summary missing"))?;
        assert_eq!(summary.workflow_id, workflow_id());
        // The seeded history records no namespace attribute, so durable
        // ownership verification must reject targeted access with NotFound:
        // a missing ownership attribute is indistinguishable from a
        // nonexistent workflow (anti-existence-leak), and NamespaceDenied is
        // reserved for callers without a grant for the requested namespace.
        assert_eq!(
            resolver
                .verify_workflow_ownership(NAMESPACE, &workflow_id())
                .await
                .err()
                .map(|error| error.to_wire_error().code),
            Some(WireErrorCode::NotFound)
        );
        Ok(())
    }

    fn apply_metadata(
        metadata: &mut tonic::metadata::MetadataMap,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Bearer credential accepted by the compiled authentication path: a
        // JWT minted against the fixture JWKS under `feature = "auth"`, the
        // development shared-secret token otherwise.
        #[cfg(feature = "auth")]
        let bearer = crate::auth::test_support::mint_token("alice", NAMESPACE)?;
        #[cfg(not(feature = "auth"))]
        let bearer = TOKEN.to_owned();
        metadata.insert("authorization", format!("Bearer {bearer}").parse()?);
        metadata.insert("x-aion-subject", "alice".parse()?);
        metadata.insert("x-aion-namespaces", NAMESPACE.parse()?);
        Ok(())
    }

    /// Test runtime settings with authentication enabled; under
    /// `feature = "auth"` validation runs against the [`server_state`]-injected
    /// JWKS cache, so the configured dev-secret `jwks_url` is never fetched.
    fn runtime_config() -> RuntimeConfig {
        RuntimeConfig {
            listen: ListenConfig {
                grpc: SocketAddr::from(([127, 0, 0, 1], 50051)),
                http: SocketAddr::from(([127, 0, 0, 1], 8080)),
            },
            tls: None,
            auth: AuthConfig {
                enabled: true,
                jwks_url: Some(TOKEN.to_owned()),
                jwks_refresh_seconds: 300,
            },
            dashboard: DashboardConfig {
                source: DashboardAssetSource::Embedded,
            },
            namespace: NamespaceConfig {
                mode: NamespaceMode::SharedEngine,
            },
            worker: WorkerConfig {
                heartbeat_window: std::time::Duration::from_millis(30_000),
            },
            websocket: WebSocketConfig {
                outbound_buffer_bound: 32,
                event_broadcast_capacity: Some(64),
                cluster_broadcast_capacity: Some(64),
            },
            workflow_packages: Vec::new(),
            deploy: DeployConfig::default(),
            authoring: AuthoringConfig::default(),
            dev: crate::config::DevConfig::default(),
            outbox: crate::config::OutboxConfig::default(),
            scheduler_threads: 1,
            query_timeout: Some(std::time::Duration::from_millis(10_000)),
            default_namespace: "default".to_owned(),
            drain_timeout: std::time::Duration::from_secs(30),
            metrics: MetricsConfig { enabled: true },
            owned_shards: Vec::new(),
            cors_allowed_origins: Vec::new(),
        }
    }

    fn started_event() -> Result<Event, aion_core::PayloadError> {
        Ok(Event::WorkflowStarted {
            envelope: EventEnvelope {
                seq: 1,
                recorded_at: Utc::now(),
                workflow_id: workflow_id(),
            },
            workflow_type: "fixture".to_owned(),
            input: payload()?,
            run_id: aion_core::RunId::new(uuid::Uuid::from_u128(1)),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        })
    }

    fn proto_payload() -> Result<aion_proto::ProtoPayload, aion_core::PayloadError> {
        Ok(payload()?.into())
    }

    fn payload() -> Result<Payload, aion_core::PayloadError> {
        Payload::from_json(&json!({ "fixture": "input" }))
    }

    fn workflow_id() -> WorkflowId {
        WorkflowId::new(uuid::Uuid::from_u128(1))
    }
}
