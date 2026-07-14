//! Transport-agnostic server-side authoring handler.
//!
//! `compile_and_load` is the authoring loop in one call: authorize (reusing
//! the deploy guard — new code admission is gated exactly like a deploy),
//! refuse during drain, compile and type-check the submitted Gleam source
//! through [`aion_toolchain`] (which only spawns the external `gleam` binary),
//! and on success hot-load the resulting package into the running engine via
//! `engine.load_package`. A type error returns the gleam diagnostics inline.
//!
//! Mounted only when `[authoring].gleam_path` is configured; with it absent
//! the routes do not exist, the server deploys pre-built `.aion` files only,
//! and nothing here is ever reached (CN7).

use std::path::PathBuf;
use std::sync::Arc;

use aion::EngineError;
use aion_awl_package::AwlAssembleOptions;
use aion_package::{ExtractionLimits, Package, PackageBuilder};
use aion_proto::WireError;
use aion_toolchain::{CompileRequest, ToolchainError, compile_source};
use serde::{Deserialize, Serialize};

use super::error::AuthoringApiError;
use crate::config::{AUTHORING_GLEAM_PATH_EMPTY, AUTHORING_PROJECT_ROOT_REQUIRED};
use crate::{CallerIdentity, ServerState};

/// Request to compile, type-check, and hot-load submitted Gleam source.
///
/// Strict parsing (`deny_unknown_fields`, consistent with the server config
/// surfaces): an unrecognised field is a 400, never silently ignored, so a
/// typo in the submission body fails loudly instead of being dropped.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompileSourceRequest {
    /// The Gleam workflow source written verbatim into a fresh per-submission
    /// working copy of the server's configured authoring project template,
    /// into its single entry-module file before building. The toolchain never
    /// rewrites it.
    pub source: String,
}

/// Response for a successful compile-and-hot-load.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompileSourceResponse {
    /// The workflow type (the manifest entry module) that was loaded.
    pub workflow_type: String,
    /// The content hash of the loaded package version.
    pub content_hash: String,
    /// The deployed (content-hash-namespaced) entry module name.
    pub deployed_entry_module: String,
    /// The entry function spawned for this version.
    pub entry_function: String,
    /// True when this call registered the version; false on idempotent re-load.
    pub freshly_loaded: bool,
    /// True when this call re-pointed the type's route at the version.
    pub route_changed: bool,
}

/// Compiles, type-checks, and hot-loads submitted Gleam source.
///
/// # Errors
///
/// Returns [`AuthoringApiError::Wire`] for authorization denials and
/// misconfiguration, [`AuthoringApiError::Unavailable`] during drain or
/// engine shutdown, [`AuthoringApiError::TypeError`] (carrying the verbatim
/// gleam diagnostics) when the source does not compile, and
/// [`AuthoringApiError::Wire`] for spawn, packaging, or load failures.
pub async fn compile_and_load(
    state: &ServerState,
    caller: &CallerIdentity,
    transport: &'static str,
    request: CompileSourceRequest,
) -> Result<CompileSourceResponse, AuthoringApiError> {
    compile_and_load_with_options(
        state,
        caller,
        transport,
        request,
        AwlAssembleOptions::default(),
    )
    .await
}

/// Compiles and hot-loads submitted Gleam source while applying AWL-native
/// manifest options after the frozen project compiler has packaged it.
///
/// # Errors
///
/// Returns the same failures as [`compile_and_load`], plus a package error if
/// applying the AWL manifest timeout cannot round-trip the built archive.
pub async fn compile_and_load_with_options(
    state: &ServerState,
    caller: &CallerIdentity,
    transport: &'static str,
    request: CompileSourceRequest,
    options: AwlAssembleOptions,
) -> Result<CompileSourceResponse, AuthoringApiError> {
    authorize_mutation(state, caller, transport)?;
    let (gleam_path, template_root) = authoring_paths(state)?;

    let mut compiled = run_compile(gleam_path, template_root, request.source).await?;
    compiled.package = package_with_options(compiled.package, options)?;
    let engine = engine_handle(state)?;
    match engine.load_package(compiled.package).await {
        Ok(outcome) => {
            let workflow_type = outcome.record.workflow_type().to_owned();
            let content_hash = outcome.record.version().to_string();
            tracing::info!(
                operation = "authoring.compile",
                subject = caller.subject(),
                grant_source = caller.grant_source().label(),
                transport,
                workflow_type = %workflow_type,
                content_hash = %content_hash,
                outcome = "loaded",
                freshly_loaded = outcome.freshly_loaded,
                route_changed = outcome.route_changed,
                "authoring compile-and-load applied"
            );
            Ok(CompileSourceResponse {
                workflow_type,
                content_hash,
                deployed_entry_module: outcome.record.deployed_entry_module().to_owned(),
                entry_function: outcome.record.entry_function().to_owned(),
                freshly_loaded: outcome.freshly_loaded,
                route_changed: outcome.route_changed,
            })
        }
        Err(error) => Err(map_load_failure(caller, transport, error)),
    }
}

pub(crate) fn package_with_options(
    package: Package,
    options: AwlAssembleOptions,
) -> Result<Package, AuthoringApiError> {
    let Some(timeout) = options.timeout else {
        return Ok(package);
    };
    let mut manifest = package.manifest().clone();
    manifest.timeout = timeout;
    let source = package
        .source()
        .iter()
        .map(|(name, bytes)| (name.clone(), bytes.clone()));
    let bytes = PackageBuilder::with_source(manifest, package.beams().clone(), source)
        .write_to_bytes()
        .map_err(|error| package_options_error(&error))?;
    Package::load_from_bytes(bytes, ExtractionLimits::unbounded())
        .map_err(|error| package_options_error(&error))
}

fn package_options_error(error: &aion_package::PackageError) -> AuthoringApiError {
    AuthoringApiError::Wire(
        WireError::invalid_input(format!(
            "AWL manifest options could not be applied: {error}"
        ))
        .with_error_type("Package"),
    )
}

/// Authorization plus drain gate, reusing the deploy guard: hot-loading new
/// code is new-work admission, gated exactly like a deploy mutation (ADR-002:
/// no second authorization mechanism).
fn authorize_mutation(
    state: &ServerState,
    caller: &CallerIdentity,
    transport: &'static str,
) -> Result<(), AuthoringApiError> {
    let guard = state.deploy_guard();
    if let Err(error) = guard.authorize(caller) {
        let wire = error.to_wire_error();
        tracing::warn!(
            operation = "authoring.compile",
            subject = caller.subject(),
            grant_source = caller.grant_source().label(),
            transport,
            reason = %wire.message,
            "authoring operation denied"
        );
        return Err(AuthoringApiError::Wire(wire));
    }
    if state.drain_state().is_draining() {
        return Err(AuthoringApiError::Unavailable(WireError::backend(
            "server is draining and not accepting authoring submissions",
        )));
    }
    Ok(())
}

/// Resolves the operator-configured authoring paths, failing loudly if the
/// surface was mounted without them (a wiring bug, never a caller error).
fn authoring_paths(state: &ServerState) -> Result<(PathBuf, PathBuf), AuthoringApiError> {
    let authoring = &state.runtime_config().authoring;
    let Some(gleam_path) = authoring.gleam_path.clone() else {
        return Err(AuthoringApiError::Wire(WireError::backend(
            AUTHORING_GLEAM_PATH_EMPTY,
        )));
    };
    let Some(project_root) = authoring.project_root.clone() else {
        return Err(AuthoringApiError::Wire(WireError::backend(
            AUTHORING_PROJECT_ROOT_REQUIRED,
        )));
    };
    Ok((gleam_path, project_root))
}

/// Runs the synchronous, multi-second compile-and-package off the async
/// runtime in a blocking task, then maps the toolchain outcome onto the
/// authoring wire classes.
///
/// The toolchain stages its own per-submission working copy of the read-only
/// `template_root`, so concurrent blocking tasks never collide on the template.
async fn run_compile(
    gleam_path: PathBuf,
    template_root: PathBuf,
    source: String,
) -> Result<aion_toolchain::CompiledWorkflow, AuthoringApiError> {
    let join = tokio::task::spawn_blocking(move || {
        compile_source(&CompileRequest {
            template_root: &template_root,
            gleam_path: &gleam_path,
            source: &source,
        })
    })
    .await;
    match join {
        Ok(Ok(compiled)) => Ok(compiled),
        Ok(Err(error)) => Err(map_toolchain_error(error)),
        Err(join_error) => Err(AuthoringApiError::Wire(WireError::backend(format!(
            "authoring compile task failed to run: {join_error}"
        )))),
    }
}

/// Maps a toolchain failure onto the authoring wire classes.
///
/// A type error is the inline 400; a spawn failure or packaging fault is a
/// backend/invalid-input wire error naming the cause.
fn map_toolchain_error(error: ToolchainError) -> AuthoringApiError {
    match error {
        ToolchainError::TypeCheck { diagnostics } => AuthoringApiError::TypeError(diagnostics),
        ToolchainError::GleamSpawn { .. } | ToolchainError::Io { .. } => {
            // Operator-side faults (binary unspawnable, project filesystem
            // unwritable): backend errors, not caller-correctable input.
            AuthoringApiError::Wire(
                WireError::backend(error.to_string()).with_error_type("Toolchain"),
            )
        }
        ToolchainError::Packaging(_) | ToolchainError::InvalidProject { .. } => {
            // The source compiled but the project could not be assembled, or
            // the project layout is unusable: a configuration/input problem.
            AuthoringApiError::Wire(
                WireError::invalid_input(error.to_string()).with_error_type("Toolchain"),
            )
        }
    }
}

/// Maps an engine load failure onto the authoring wire classes, mirroring the
/// deploy load mapping.
fn map_load_failure(
    caller: &CallerIdentity,
    transport: &'static str,
    error: EngineError,
) -> AuthoringApiError {
    let mapped = match error {
        EngineError::ShuttingDown => AuthoringApiError::Unavailable(
            WireError::backend(error.to_string()).with_error_type("ShuttingDown"),
        ),
        EngineError::Load { .. } => AuthoringApiError::Wire(
            WireError::invalid_input(error.to_string()).with_error_type("Load"),
        ),
        EngineError::Package(_) => AuthoringApiError::Wire(
            WireError::invalid_input(error.to_string()).with_error_type("Package"),
        ),
        other => AuthoringApiError::Wire(crate::ServerError::from(other).to_wire_error()),
    };
    tracing::info!(
        operation = "authoring.compile",
        subject = caller.subject(),
        grant_source = caller.grant_source().label(),
        transport,
        outcome = mapped.outcome(),
        "authoring compile-and-load refused at hot-load"
    );
    mapped
}

/// Borrows the engine handle for the authorized authoring operation, reusing
/// the deploy guard's engine accessor.
fn engine_handle(state: &ServerState) -> Result<Arc<aion::Engine>, AuthoringApiError> {
    state
        .deploy_guard()
        .engine()
        .map(Arc::clone)
        .map_err(|error| AuthoringApiError::Wire(error.to_wire_error()))
}
