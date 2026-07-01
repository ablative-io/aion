//! tonic workflow service adapter.

/// Caller-identity extraction from gRPC request metadata.
mod auth;
/// Codec conversions between the generated wire messages and `aion-proto` types.
mod convert;
/// Cluster request routing/resolution (steered start + forward-or-local).
#[cfg(feature = "haematite-backend")]
mod routing_resolve;
/// Wire-error-to-tonic-Status mapping.
mod status;

pub(crate) use auth::caller_from_metadata;
pub(crate) use status::{status_from_wire_error, status_with_code};

use aion_proto::generated::{self, workflow_service_server::WorkflowServiceServer};
use tonic::{Request, Response, Status};

use crate::{CallerIdentity, ServerState, api::handlers, api::schedule_handlers};
#[cfg(feature = "haematite-backend")]
use convert::decode_workflow_id;
use convert::{
    decode_cancel_request, decode_count_request, decode_create_schedule_request,
    decode_describe_request, decode_list_request, decode_list_schedules_request,
    decode_query_request, decode_reopen_request, decode_schedule_id_request, decode_signal_request,
    decode_start_request, decode_update_schedule_request, encode_cancel_response,
    encode_count_response, encode_create_schedule_response, encode_delete_schedule_response,
    encode_describe_response, encode_describe_schedule_response, encode_list_response,
    encode_list_schedules_response, encode_pause_schedule_response, encode_query_response,
    encode_reopen_response, encode_resume_schedule_response, encode_signal_response,
    encode_start_response, encode_update_schedule_response,
};
#[cfg(feature = "haematite-backend")]
use routing_resolve::{RouteResolution, StartResolution};

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
            // Minted-on-use safety net (Phase 1 S6): mint/gate the authorized
            // namespace before the engine start, sharing the SAME policy the
            // worker-registration seam applies. Auth-scoped (`guard.scope` runs
            // inside the handler); placement is the orthogonal steered-start id.
            let minter = self.state.namespace_minter();
            let response = handlers::start_with_placement(
                self.state.namespace_guard(),
                &caller,
                decode_start_request(inner),
                placement,
                Some(&minter),
            )
            .await
            .map_err(status_from_wire_error)?;
            return Ok(Response::new(encode_start_response(response)));
        }
        #[cfg(not(feature = "haematite-backend"))]
        {
            let placement: Option<aion_core::WorkflowId> = None;
            // Minted-on-use safety net (Phase 1 S6): mint/gate the authorized
            // namespace before the engine start, sharing the SAME policy the
            // worker-registration seam applies.
            let minter = self.state.namespace_minter();
            let response = handlers::start_with_placement(
                self.state.namespace_guard(),
                &caller,
                decode_start_request(request.into_inner()),
                placement,
                Some(&minter),
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

    async fn reopen(
        &self,
        request: Request<generated::ReopenRequest>,
    ) -> Result<Response<generated::ReopenResponse>, Status> {
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
                    ForwardRequest::Reopen(inner.clone()),
                )
                .await
            {
                RouteResolution::Reject(status) => return Err(status),
                RouteResolution::Reply(ForwardReply::Reopen(reply)) => {
                    return Ok(Response::new(reply));
                }
                RouteResolution::Reply(_) => {
                    return Err(Status::internal("forwarder returned a mismatched reply"));
                }
                RouteResolution::Local => {
                    let response = handlers::reopen(
                        self.state.namespace_guard(),
                        &caller,
                        decode_reopen_request(inner),
                    )
                    .await
                    .map_err(status_from_wire_error)?;
                    return Ok(Response::new(encode_reopen_response(response)));
                }
            }
        }
        #[cfg(not(feature = "haematite-backend"))]
        {
            let response = handlers::reopen(
                self.state.namespace_guard(),
                &caller,
                decode_reopen_request(request.into_inner()),
            )
            .await
            .map_err(status_from_wire_error)?;
            Ok(Response::new(encode_reopen_response(response)))
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

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::Arc};

    use aion::EngineBuilder;
    use aion_core::{Event, EventEnvelope, Payload, WorkflowId, WorkflowStatus};
    use aion_proto::{
        ProtoWireError, WireError, WireErrorCode,
        convert::{decode_core_value, encode_core_value},
        generated::workflow_service_server::WorkflowService,
    };
    use aion_store::{
        EventStore, InMemoryStore, WriteToken,
        visibility::{VisibilityRecord, VisibilityStore},
    };
    use chrono::Utc;
    use prost::Message;
    use serde_json::json;
    use tonic::{Code, Request};

    use super::convert::{decode_envelope, encode_envelope, encode_payload};
    use super::*;
    use crate::{
        NamespaceResolver,
        config::{
            AuthConfig, AuthoringConfig, DeployConfig, ListenConfig, MetricsConfig,
            NamespaceConfig, NamespaceMode, OpsConsoleAssetSource, OpsConsoleConfig, RuntimeConfig,
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
                failed_step: None,
                failure_reason: None,
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
            task_queue: None,
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

    /// Reopening a terminal-Completed workflow over the gRPC service surfaces
    /// the engine's `InvalidState` precondition as tonic `FailedPrecondition`
    /// carrying the typed `InvalidState` detail (AO-007 C35/C38).
    #[tokio::test]
    async fn in_process_tonic_reopen_completed_is_failed_precondition_invalid_state()
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
        // A terminal-Completed run whose history records its namespace, so the
        // guard's durable-ownership verification passes and the request reaches
        // the engine reopen op (which rejects Completed with InvalidState).
        store
            .append(
                WriteToken::recorder(),
                &workflow_id(),
                &[
                    started_event()?,
                    Event::SearchAttributesUpdated {
                        envelope: EventEnvelope {
                            seq: 2,
                            recorded_at: Utc::now(),
                            workflow_id: workflow_id(),
                        },
                        workflow_id: workflow_id(),
                        attributes: std::collections::HashMap::from([(
                            crate::namespace::NAMESPACE_ATTRIBUTE.to_owned(),
                            aion_core::SearchAttributeValue::String(NAMESPACE.to_owned()),
                        )]),
                    },
                    Event::WorkflowCompleted {
                        envelope: EventEnvelope {
                            seq: 3,
                            recorded_at: Utc::now(),
                            workflow_id: workflow_id(),
                        },
                        result: payload()?,
                    },
                ],
                0,
            )
            .await?;
        let resolver = NamespaceResolver::from_config(
            crate::config::NamespaceConfig {
                mode: NamespaceMode::SharedEngine,
            },
            engine,
        );
        let state = server_state(resolver, runtime_config()).await?;
        let service = WorkflowGrpcService::new(state);

        let mut reopen = Request::new(generated::ReopenRequest {
            namespace: NAMESPACE.to_owned(),
            workflow_id: Some(generated::WorkflowId {
                uuid: workflow_id().to_string(),
            }),
            run_id: None,
        });
        apply_metadata(reopen.metadata_mut())?;
        let status = service
            .reopen(reopen)
            .await
            .err()
            .ok_or_else(|| WireError::backend("expected a reopen precondition error"))?;
        assert_eq!(status.code(), Code::FailedPrecondition);
        let detail = ProtoWireError::decode(status.details())?;
        assert_eq!(detail.error_type.as_deref(), Some("InvalidState"));
        assert_eq!(
            detail.code,
            aion_proto::ProtoWireErrorCode::InvalidState as i32
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
            ops_console: OpsConsoleConfig {
                source: OpsConsoleAssetSource::Embedded,
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
            auto_create: crate::config::AutoCreate::Open,
            max_in_flight_activities: crate::config::DEFAULT_MAX_IN_FLIGHT_ACTIVITIES,
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
