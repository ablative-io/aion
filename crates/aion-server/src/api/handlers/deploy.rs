//! Shared operator-deploy handlers used by both transports.
//!
//! Every operation authorizes via [`crate::deploy::DeployGuard`] before any
//! handler logic runs, then maps engine outcomes onto the deploy wire
//! contract: `deploy_denied` for authorization, `version_pinned` for
//! route-active/pinned refusals, `invalid_input` for malformed archives and
//! the same-hash-different-manifest tripwire, `not_found` for unknown
//! `(type, version)`, and 503/`Unavailable` for drain/shutdown windows.
//! Mutations emit one structured audit line and the deploy metrics.

use aion::EngineError;
use aion_package::{ContentHash, Package};
use aion_proto::{
    ProtoListVersionsResponse, ProtoLoadPackageResponse, ProtoRouteVersionRequest,
    ProtoRouteVersionResponse, ProtoUnloadVersionRequest, ProtoUnloadVersionResponse,
    ProtoWorkflowVersion, WireError,
};

use crate::config::DEPLOY_MAX_ARCHIVE_BYTES_REQUIRED;
use crate::{CallerIdentity, ServerState};

/// Deploy failure classes the transports must render distinctly: 503 vs 413
/// vs the regular wire-code mapping.
#[derive(Debug)]
pub enum DeployApiError {
    /// The server is draining or the engine is shutting down (503/`Unavailable`).
    Unavailable(WireError),
    /// The uploaded archive exceeds `deploy.max_archive_bytes` (413/`InvalidArgument`).
    ArchiveTooLarge(WireError),
    /// Mapped wire failure rendered through the standard code tables.
    Wire(WireError),
}

impl DeployApiError {
    /// Borrow the carried wire error regardless of class.
    #[must_use]
    pub const fn wire(&self) -> &WireError {
        match self {
            Self::Unavailable(wire) | Self::ArchiveTooLarge(wire) | Self::Wire(wire) => wire,
        }
    }
}

/// Handles a deploy archive upload (`LoadPackage` / `POST /deploy/packages`).
///
/// Idempotency is specified behavior: re-sending a resident archive succeeds
/// with `freshly_loaded = false`, and `route_changed` reports whether the
/// call re-pointed routing.
///
/// # Errors
///
/// Returns [`DeployApiError`] for authorization denials, drain/shutdown
/// refusals, oversized or malformed archives, the manifest-mismatch
/// tripwire, and engine failures.
pub async fn load_package(
    state: &ServerState,
    caller: &CallerIdentity,
    transport: &'static str,
    archive: Vec<u8>,
) -> Result<ProtoLoadPackageResponse, DeployApiError> {
    authorize_mutation(state, caller, transport, "deploy.load")?;
    enforce_archive_ceiling(state, archive.len())?;

    let package = match Package::load_from_bytes(&archive) {
        Ok(package) => package,
        Err(error) => {
            let wire = WireError::invalid_input(format!("archive rejected: {error}"))
                .with_error_type("Package");
            return Err(refused(state, caller, transport, "deploy.load", None, wire));
        }
    };
    let engine = engine_handle(state)?;
    match engine.load_package(package).await {
        Ok(outcome) => {
            let workflow_type = outcome.record.workflow_type().to_owned();
            let content_hash = outcome.record.version().to_string();
            let audit_outcome = if outcome.freshly_loaded {
                "loaded"
            } else {
                "idempotent"
            };
            tracing::info!(
                operation = "deploy.load",
                subject = caller.subject(),
                grant_source = caller.grant_source().label(),
                transport,
                workflow_type = %workflow_type,
                content_hash = %content_hash,
                outcome = audit_outcome,
                freshly_loaded = outcome.freshly_loaded,
                route_changed = outcome.route_changed,
                "deploy mutation applied"
            );
            record_mutation_metrics(state, "deploy.load", audit_outcome, &workflow_type);
            Ok(ProtoLoadPackageResponse {
                workflow_type,
                content_hash,
                deployed_entry_module: outcome.record.deployed_entry_module().to_owned(),
                entry_function: outcome.record.entry_function().to_owned(),
                freshly_loaded: outcome.freshly_loaded,
                route_changed: outcome.route_changed,
            })
        }
        Err(error) => Err(map_engine_refusal(
            state,
            caller,
            transport,
            "deploy.load",
            None,
            error,
        )),
    }
}

/// Handles the deploy read model (`ListVersions` / `GET /deploy/versions`).
///
/// Listing keeps serving during drain: it is the operator's view of a
/// rollout, not new work admission.
///
/// # Errors
///
/// Returns [`DeployApiError`] for authorization denials and engine failures.
pub fn list_versions(
    state: &ServerState,
    caller: &CallerIdentity,
    transport: &'static str,
) -> Result<ProtoListVersionsResponse, DeployApiError> {
    let guard = state.deploy_guard();
    if let Err(error) = guard.authorize(caller) {
        return Err(denied(state, caller, transport, "deploy.list", &error));
    }
    let engine = engine_handle(state)?;
    let versions = engine
        .list_workflow_versions()
        .map_err(|error| DeployApiError::Wire(crate::ServerError::from(error).to_wire_error()))?;
    Ok(ProtoListVersionsResponse {
        versions: versions
            .into_iter()
            .map(|info| ProtoWorkflowVersion {
                workflow_type: info.workflow_type,
                content_hash: info.content_hash.to_string(),
                deployed_entry_module: info.deployed_entry_module,
                entry_function: info.entry_function,
                manifest_version: info.manifest_version.as_str().to_owned(),
                loaded_at: info.loaded_at.to_rfc3339(),
                route_active: info.route_active,
            })
            .collect(),
    })
}

/// Handles a route re-point (`RouteVersion` / `POST /deploy/route`).
///
/// # Errors
///
/// Returns [`DeployApiError`] for authorization denials, drain/shutdown
/// refusals, malformed hashes, unknown versions, and engine failures.
pub async fn route_version(
    state: &ServerState,
    caller: &CallerIdentity,
    transport: &'static str,
    request: ProtoRouteVersionRequest,
) -> Result<ProtoRouteVersionResponse, DeployApiError> {
    authorize_mutation(state, caller, transport, "deploy.route")?;
    let (workflow_type, version) = decode_version_target(
        state,
        caller,
        transport,
        "deploy.route",
        &request.workflow_type,
        &request.content_hash,
    )?;
    let engine = engine_handle(state)?;
    match engine
        .route_workflow_version(&workflow_type, &version)
        .await
    {
        Ok(()) => {
            tracing::info!(
                operation = "deploy.route",
                subject = caller.subject(),
                grant_source = caller.grant_source().label(),
                transport,
                workflow_type = %workflow_type,
                content_hash = %version,
                outcome = "rerouted",
                "deploy mutation applied"
            );
            record_mutation_metrics(state, "deploy.route", "rerouted", &workflow_type);
            Ok(ProtoRouteVersionResponse {})
        }
        Err(error) => Err(map_engine_refusal(
            state,
            caller,
            transport,
            "deploy.route",
            Some((&workflow_type, &version)),
            error,
        )),
    }
}

/// Handles a version unload (`UnloadVersion` / `POST /deploy/unload`).
///
/// # Errors
///
/// Returns [`DeployApiError`] for authorization denials, drain/shutdown
/// refusals, malformed hashes, unknown versions, pinned/route-active
/// refusals, and engine failures.
pub async fn unload_version(
    state: &ServerState,
    caller: &CallerIdentity,
    transport: &'static str,
    request: ProtoUnloadVersionRequest,
) -> Result<ProtoUnloadVersionResponse, DeployApiError> {
    authorize_mutation(state, caller, transport, "deploy.unload")?;
    let (workflow_type, version) = decode_version_target(
        state,
        caller,
        transport,
        "deploy.unload",
        &request.workflow_type,
        &request.content_hash,
    )?;
    let engine = engine_handle(state)?;
    match engine
        .unload_workflow_version(&workflow_type, &version)
        .await
    {
        Ok(()) => {
            tracing::info!(
                operation = "deploy.unload",
                subject = caller.subject(),
                grant_source = caller.grant_source().label(),
                transport,
                workflow_type = %workflow_type,
                content_hash = %version,
                outcome = "unloaded",
                "deploy mutation applied"
            );
            record_mutation_metrics(state, "deploy.unload", "unloaded", &workflow_type);
            Ok(ProtoUnloadVersionResponse {})
        }
        Err(error) => Err(map_engine_refusal(
            state,
            caller,
            transport,
            "deploy.unload",
            Some((&workflow_type, &version)),
            error,
        )),
    }
}

/// Authorization plus drain gate shared by every deploy mutation.
fn authorize_mutation(
    state: &ServerState,
    caller: &CallerIdentity,
    transport: &'static str,
    operation: &'static str,
) -> Result<(), DeployApiError> {
    let guard = state.deploy_guard();
    if let Err(error) = guard.authorize(caller) {
        return Err(denied(state, caller, transport, operation, &error));
    }
    if state.drain_state().is_draining() {
        return Err(DeployApiError::Unavailable(WireError::backend(
            "server is draining and not accepting deploy mutations",
        )));
    }
    Ok(())
}

/// Records, logs, and wraps an authorization denial. Denied calls never
/// reach the engine.
fn denied(
    state: &ServerState,
    caller: &CallerIdentity,
    transport: &'static str,
    operation: &'static str,
    error: &crate::ServerError,
) -> DeployApiError {
    let wire = error.to_wire_error();
    tracing::warn!(
        operation,
        subject = caller.subject(),
        grant_source = caller.grant_source().label(),
        transport,
        reason = %wire.message,
        "deploy operation denied"
    );
    if let Some(metrics) = state.metrics() {
        metrics.deploy_denied(transport);
    }
    DeployApiError::Wire(wire)
}

/// Enforces the operator-configured archive ceiling, naming the config key.
fn enforce_archive_ceiling(state: &ServerState, archive_len: usize) -> Result<(), DeployApiError> {
    let Some(limit) = state.runtime_config().deploy.max_archive_bytes else {
        // The deploy surface is only mounted when validation proved the
        // ceiling present; reaching this is a wiring bug, never a caller
        // error, and it must fail loudly rather than admit unbounded bodies.
        return Err(DeployApiError::Wire(WireError::backend(
            DEPLOY_MAX_ARCHIVE_BYTES_REQUIRED,
        )));
    };
    if archive_len as u64 > limit {
        return Err(DeployApiError::ArchiveTooLarge(WireError::invalid_input(
            format!(
                "archive is {archive_len} bytes, exceeding the deploy.max_archive_bytes limit of {limit} bytes; raise deploy.max_archive_bytes (or AION_DEPLOY_MAX_ARCHIVE_BYTES) if this package size is intended"
            ),
        )));
    }
    Ok(())
}

/// Decodes a `(workflow_type, content_hash)` target, refusing malformed input.
fn decode_version_target(
    state: &ServerState,
    caller: &CallerIdentity,
    transport: &'static str,
    operation: &'static str,
    workflow_type: &str,
    content_hash: &str,
) -> Result<(String, ContentHash), DeployApiError> {
    if workflow_type.is_empty() {
        let wire = WireError::invalid_input("workflow_type must not be empty");
        return Err(refused(state, caller, transport, operation, None, wire));
    }
    match content_hash.parse::<ContentHash>() {
        Ok(version) => Ok((workflow_type.to_owned(), version)),
        Err(error) => {
            let wire = WireError::invalid_input(format!(
                "content_hash `{content_hash}` is not a canonical content hash: {error}"
            ));
            Err(refused(state, caller, transport, operation, None, wire))
        }
    }
}

/// Maps an engine failure onto the deploy wire contract, emitting the audit
/// line and refusal metrics.
fn map_engine_refusal(
    state: &ServerState,
    caller: &CallerIdentity,
    transport: &'static str,
    operation: &'static str,
    target: Option<(&str, &ContentHash)>,
    error: EngineError,
) -> DeployApiError {
    let mapped = match error {
        EngineError::ShuttingDown => DeployApiError::Unavailable(
            WireError::backend(error.to_string()).with_error_type("ShuttingDown"),
        ),
        // On the deploy path archive/validation/collision/registration
        // failures are caller-correctable input problems, not backend faults.
        EngineError::Load { .. } => DeployApiError::Wire(
            WireError::invalid_input(error.to_string()).with_error_type("Load"),
        ),
        EngineError::Package(_) => DeployApiError::Wire(
            WireError::invalid_input(error.to_string()).with_error_type("Package"),
        ),
        // UnknownVersion -> not_found, VersionPinned/RouteActive ->
        // version_pinned, ManifestMismatch -> invalid_input via the central
        // ServerError mapping; refusal prose passes through verbatim.
        other => DeployApiError::Wire(crate::ServerError::from(other).to_wire_error()),
    };
    let wire = mapped.wire();
    let outcome = refusal_outcome(&mapped);
    tracing::info!(
        operation,
        subject = caller.subject(),
        grant_source = caller.grant_source().label(),
        transport,
        workflow_type = target.map(|(workflow_type, _)| workflow_type),
        content_hash = target.map(|(_, version)| version.to_string()).as_deref(),
        outcome,
        reason = %wire.message,
        "deploy mutation refused"
    );
    if let Some(metrics) = state.metrics() {
        metrics.deploy_operation(operation, outcome);
    }
    mapped
}

/// Records, logs, and wraps an adapter-level refusal (malformed input).
fn refused(
    state: &ServerState,
    caller: &CallerIdentity,
    transport: &'static str,
    operation: &'static str,
    target: Option<(&str, &ContentHash)>,
    wire: WireError,
) -> DeployApiError {
    let mapped = DeployApiError::Wire(wire);
    let outcome = refusal_outcome(&mapped);
    tracing::info!(
        operation,
        subject = caller.subject(),
        grant_source = caller.grant_source().label(),
        transport,
        workflow_type = target.map(|(workflow_type, _)| workflow_type),
        content_hash = target.map(|(_, version)| version.to_string()).as_deref(),
        outcome,
        reason = %mapped.wire().message,
        "deploy mutation refused"
    );
    if let Some(metrics) = state.metrics() {
        metrics.deploy_operation(operation, outcome);
    }
    mapped
}

/// Stable refusal-class label for audit lines and the outcome metric.
fn refusal_outcome(error: &DeployApiError) -> &'static str {
    match error {
        DeployApiError::Unavailable(_) => "unavailable",
        DeployApiError::ArchiveTooLarge(_) | DeployApiError::Wire(_) => error.wire().code.as_str(),
    }
}

fn engine_handle(state: &ServerState) -> Result<std::sync::Arc<aion::Engine>, DeployApiError> {
    state
        .deploy_guard()
        .engine()
        .map(std::sync::Arc::clone)
        .map_err(|error| DeployApiError::Wire(error.to_wire_error()))
}

/// Counter + gauge updates for one applied mutation. The gauge is set from
/// the post-operation listing for the affected workflow type (0 when the
/// last version of a type was unloaded).
fn record_mutation_metrics(
    state: &ServerState,
    operation: &'static str,
    outcome: &'static str,
    workflow_type: &str,
) {
    let Some(metrics) = state.metrics() else {
        return;
    };
    metrics.deploy_operation(operation, outcome);
    let Ok(engine) = state.deploy_guard().engine().map(std::sync::Arc::clone) else {
        return;
    };
    match engine.list_workflow_versions() {
        Ok(versions) => {
            let count = versions
                .iter()
                .filter(|info| info.workflow_type == workflow_type)
                .count();
            let count = i64::try_from(count).unwrap_or(i64::MAX);
            metrics.set_loaded_workflow_versions(workflow_type, count);
        }
        Err(error) => {
            tracing::warn!(
                operation,
                workflow_type,
                %error,
                "post-operation version listing failed; loaded-version gauge not updated"
            );
        }
    }
}
