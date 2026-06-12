//! tonic `DeployService` adapter over the shared deploy handlers.
//!
//! Added to the existing gRPC listener only when `[deploy].enabled` is true;
//! a disabled surface answers `Unimplemented` (tonic's unknown-service
//! response), exposing no deploy code path at all.

use aion_proto::{
    ProtoRouteVersionRequest, ProtoUnloadVersionRequest,
    generated::{self, deploy_service_server::DeployServiceServer},
};
use tonic::{Code, Request, Response, Status};

use super::grpc::{caller_from_metadata, status_from_wire_error, status_with_code};
use crate::api::handlers::deploy::{self, DeployApiError};
use crate::config::DEPLOY_MAX_ARCHIVE_BYTES_REQUIRED;
use crate::{CallerIdentity, ServerState};

const TRANSPORT: &str = "grpc";

/// Proto-framing allowance over the archive ceiling for the unary
/// `LoadPackageRequest`: the message wraps the archive bytes in one
/// length-delimited field (tag byte plus a length varint of at most ten
/// bytes), so a conformant archive of exactly `max_archive_bytes` always
/// decodes while anything meaningfully larger is refused at the transport.
const LOAD_PACKAGE_FRAMING_ALLOWANCE: usize = 64;

/// Cloneable tonic implementation of the operator deploy service.
#[derive(Clone)]
pub struct DeployGrpcService {
    state: ServerState,
}

impl DeployGrpcService {
    /// Build a tonic deploy service from shared server state.
    #[must_use]
    pub const fn new(state: ServerState) -> Self {
        Self { state }
    }

    async fn caller<T>(&self, request: &Request<T>) -> Result<CallerIdentity, Status> {
        caller_from_metadata(request.metadata(), &self.state).await
    }
}

/// Construct the generated tonic server wrapper with a decode ceiling sized
/// from `deploy.max_archive_bytes`.
///
/// # Errors
///
/// Returns [`crate::ServerError::Config`] when the deploy surface is enabled
/// without the required `deploy.max_archive_bytes` (defense in depth; config
/// validation refuses this earlier).
pub fn deploy_service(
    state: ServerState,
) -> Result<DeployServiceServer<DeployGrpcService>, crate::ServerError> {
    let Some(limit) = state.runtime_config().deploy.max_archive_bytes else {
        return Err(crate::ServerError::Config {
            message: DEPLOY_MAX_ARCHIVE_BYTES_REQUIRED.to_owned(),
        });
    };
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    Ok(DeployServiceServer::new(DeployGrpcService::new(state))
        .max_decoding_message_size(limit.saturating_add(LOAD_PACKAGE_FRAMING_ALLOWANCE)))
}

#[tonic::async_trait]
impl generated::deploy_service_server::DeployService for DeployGrpcService {
    async fn load_package(
        &self,
        request: Request<generated::LoadPackageRequest>,
    ) -> Result<Response<generated::LoadPackageResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = deploy::load_package(
            &self.state,
            &caller,
            TRANSPORT,
            request.into_inner().archive,
        )
        .await
        .map_err(status_from_deploy_error)?;
        Ok(Response::new(generated::LoadPackageResponse {
            workflow_type: response.workflow_type,
            content_hash: response.content_hash,
            deployed_entry_module: response.deployed_entry_module,
            entry_function: response.entry_function,
            freshly_loaded: response.freshly_loaded,
            route_changed: response.route_changed,
        }))
    }

    async fn list_versions(
        &self,
        request: Request<generated::ListVersionsRequest>,
    ) -> Result<Response<generated::ListVersionsResponse>, Status> {
        let caller = self.caller(&request).await?;
        let response = deploy::list_versions(&self.state, &caller, TRANSPORT)
            .map_err(status_from_deploy_error)?;
        Ok(Response::new(generated::ListVersionsResponse {
            versions: response
                .versions
                .into_iter()
                .map(|version| generated::WorkflowVersion {
                    workflow_type: version.workflow_type,
                    content_hash: version.content_hash,
                    deployed_entry_module: version.deployed_entry_module,
                    entry_function: version.entry_function,
                    manifest_version: version.manifest_version,
                    loaded_at: version.loaded_at,
                    route_active: version.route_active,
                })
                .collect(),
        }))
    }

    async fn route_version(
        &self,
        request: Request<generated::RouteVersionRequest>,
    ) -> Result<Response<generated::RouteVersionResponse>, Status> {
        let caller = self.caller(&request).await?;
        let inner = request.into_inner();
        deploy::route_version(
            &self.state,
            &caller,
            TRANSPORT,
            ProtoRouteVersionRequest {
                workflow_type: inner.workflow_type,
                content_hash: inner.content_hash,
            },
        )
        .await
        .map_err(status_from_deploy_error)?;
        Ok(Response::new(generated::RouteVersionResponse {}))
    }

    async fn unload_version(
        &self,
        request: Request<generated::UnloadVersionRequest>,
    ) -> Result<Response<generated::UnloadVersionResponse>, Status> {
        let caller = self.caller(&request).await?;
        let inner = request.into_inner();
        deploy::unload_version(
            &self.state,
            &caller,
            TRANSPORT,
            ProtoUnloadVersionRequest {
                workflow_type: inner.workflow_type,
                content_hash: inner.content_hash,
            },
        )
        .await
        .map_err(status_from_deploy_error)?;
        Ok(Response::new(generated::UnloadVersionResponse {}))
    }
}

/// Deploy failure mapping: drain/shutdown → `Unavailable`, oversized archive
/// → `InvalidArgument`, everything else through the standard code table.
/// The typed `ProtoWireError` detail rides every status.
fn status_from_deploy_error(error: DeployApiError) -> Status {
    match error {
        DeployApiError::Unavailable(wire) => status_with_code(Code::Unavailable, wire),
        DeployApiError::ArchiveTooLarge(wire) => status_with_code(Code::InvalidArgument, wire),
        DeployApiError::Wire(wire) => status_from_wire_error(wire),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion::EngineBuilder;
    use aion_proto::{ProtoWireError, WireError, WireErrorCode, generated};
    use aion_store::{EventStore, InMemoryStore};
    use prost::Message as _;
    use tonic::{Code, Request, Status};

    use super::DeployGrpcService;
    use crate::config::{
        AuthConfig, DashboardAssetSource, DashboardConfig, DeployConfig, ListenConfig,
        MetricsConfig, NamespaceConfig, NamespaceMode, RuntimeConfig, WebSocketConfig,
        WorkerConfig,
    };
    use crate::{
        NamespaceResolver, ServerState, StaticScheduleNamespaces, StaticWorkflowNamespaces,
    };

    /// Decode the typed `ProtoWireError` detail riding a deploy status.
    fn decode_detail(status: &Status) -> Result<WireError, Box<dyn std::error::Error>> {
        let proto = ProtoWireError::decode(status.details())?;
        Ok(WireError::try_from(proto)?)
    }

    fn runtime_config() -> RuntimeConfig {
        RuntimeConfig {
            listen: ListenConfig {
                grpc: std::net::SocketAddr::from(([127, 0, 0, 1], 50051)),
                http: std::net::SocketAddr::from(([127, 0, 0, 1], 8080)),
            },
            tls: None,
            auth: AuthConfig {
                enabled: false,
                jwks_url: None,
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
            },
            workflow_packages: Vec::new(),
            deploy: DeployConfig::default(),
            scheduler_threads: 1,
            query_timeout: Some(std::time::Duration::from_millis(10_000)),
            default_namespace: "default".to_owned(),
            drain_timeout: std::time::Duration::from_secs(30),
            metrics: MetricsConfig { enabled: true },
        }
    }

    async fn deploy_state() -> Result<ServerState, Box<dyn std::error::Error>> {
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        let engine = Arc::new(
            EngineBuilder::new()
                .store_arc(store)
                .in_memory_visibility()
                .scheduler_threads(1)
                .build()
                .await?,
        );
        let resolver = NamespaceResolver::from_parts(
            NamespaceMode::SharedEngine,
            Some(engine),
            Arc::new(StaticWorkflowNamespaces::default()),
            Arc::new(StaticScheduleNamespaces::default()),
        );
        let mut config = runtime_config();
        config.deploy = DeployConfig {
            enabled: true,
            max_archive_bytes: Some(1024),
        };
        Ok(ServerState::from_parts(resolver, config))
    }

    fn granted_request<T>(message: T) -> Result<Request<T>, Box<dyn std::error::Error>> {
        let mut request = Request::new(message);
        request
            .metadata_mut()
            .insert("x-aion-subject", "ci".parse()?);
        request
            .metadata_mut()
            .insert("x-aion-deploy", "true".parse()?);
        Ok(request)
    }

    #[tokio::test]
    async fn denied_metadata_is_permission_denied_with_deploy_denied_detail()
    -> Result<(), Box<dyn std::error::Error>> {
        use generated::deploy_service_server::DeployService as _;

        let service = DeployGrpcService::new(deploy_state().await?);
        let mut request = Request::new(generated::ListVersionsRequest {});
        request
            .metadata_mut()
            .insert("x-aion-subject", "ci".parse()?);

        let status = service
            .list_versions(request)
            .await
            .err()
            .ok_or("expected denial")?;
        assert_eq!(status.code(), Code::PermissionDenied);
        let detail = decode_detail(&status)?;
        assert_eq!(detail.code, WireErrorCode::DeployDenied);
        assert!(
            detail.message.contains("x-aion-deploy"),
            "denial must hint the dev header: {}",
            detail.message
        );
        Ok(())
    }

    #[tokio::test]
    async fn granted_metadata_lists_versions() -> Result<(), Box<dyn std::error::Error>> {
        use generated::deploy_service_server::DeployService as _;

        let service = DeployGrpcService::new(deploy_state().await?);
        let response = service
            .list_versions(granted_request(generated::ListVersionsRequest {})?)
            .await?;
        assert!(response.into_inner().versions.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn oversized_archive_is_invalid_argument_naming_the_key()
    -> Result<(), Box<dyn std::error::Error>> {
        use generated::deploy_service_server::DeployService as _;

        let service = DeployGrpcService::new(deploy_state().await?);
        let status = service
            .load_package(granted_request(generated::LoadPackageRequest {
                archive: vec![0_u8; 2048],
            })?)
            .await
            .err()
            .ok_or("expected oversize refusal")?;

        assert_eq!(status.code(), Code::InvalidArgument);
        assert!(
            status.message().contains("deploy.max_archive_bytes"),
            "refusal must name the config key: {}",
            status.message()
        );
        Ok(())
    }

    #[tokio::test]
    async fn route_to_unknown_version_is_not_found() -> Result<(), Box<dyn std::error::Error>> {
        use generated::deploy_service_server::DeployService as _;

        let service = DeployGrpcService::new(deploy_state().await?);
        let status = service
            .route_version(granted_request(generated::RouteVersionRequest {
                workflow_type: "order".to_owned(),
                content_hash: "a".repeat(64),
            })?)
            .await
            .err()
            .ok_or("expected unknown-version refusal")?;

        assert_eq!(status.code(), Code::NotFound);
        let detail = decode_detail(&status)?;
        assert_eq!(detail.code, WireErrorCode::NotFound);
        assert_eq!(detail.error_type.as_deref(), Some("UnknownVersion"));
        Ok(())
    }

    #[tokio::test]
    async fn malformed_hash_is_invalid_argument() -> Result<(), Box<dyn std::error::Error>> {
        use generated::deploy_service_server::DeployService as _;

        let service = DeployGrpcService::new(deploy_state().await?);
        let status = service
            .unload_version(granted_request(generated::UnloadVersionRequest {
                workflow_type: "order".to_owned(),
                content_hash: "not-a-hash".to_owned(),
            })?)
            .await
            .err()
            .ok_or("expected malformed-hash refusal")?;

        assert_eq!(status.code(), Code::InvalidArgument);
        assert!(
            status.message().contains("not-a-hash"),
            "refusal must name the malformed hash: {}",
            status.message()
        );
        Ok(())
    }

    /// Drain: mutations refuse with `Unavailable`; the versions read model
    /// keeps serving (operators watch rollouts through it).
    #[tokio::test]
    async fn drain_refuses_mutations_but_serves_listing() -> Result<(), Box<dyn std::error::Error>>
    {
        use generated::deploy_service_server::DeployService as _;

        let state = deploy_state().await?;
        assert!(state.drain_state().begin());
        let service = DeployGrpcService::new(state);

        let status = service
            .route_version(granted_request(generated::RouteVersionRequest {
                workflow_type: "order".to_owned(),
                content_hash: "a".repeat(64),
            })?)
            .await
            .err()
            .ok_or("expected drain refusal")?;
        assert_eq!(status.code(), Code::Unavailable);
        assert!(
            status.message().contains("draining"),
            "drain refusal must be explicit: {}",
            status.message()
        );

        let listing = service
            .list_versions(granted_request(generated::ListVersionsRequest {})?)
            .await?;
        assert!(listing.into_inner().versions.is_empty());
        Ok(())
    }
}
