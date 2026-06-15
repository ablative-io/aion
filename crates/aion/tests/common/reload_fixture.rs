//! Shared two-version reload fixture for the #62 live-reload test suites.
//!
//! Version N of `aion_reload_fixture` completes with the integer N from
//! every entrypoint: `run/1` immediately, `park/1` after any mailbox
//! message, and `gated/1` after the durable signals `step` then `release`
//! (so replay resolves recorded progress deterministically).

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, RuntimeHandle, SignalRouter};
use aion_core::{Event, PackageVersion, Payload, RunId, WorkflowId};
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, ExtractionLimits, Manifest, ManifestVersion,
    Package, PackageBuilder,
};
use aion_store::EventStore;
use serde_json::json;

pub const RELOAD_MODULE: &str = "aion_reload_fixture";

/// Compiles the reload fixture returning `version` from every entrypoint.
pub fn compile_reload_beam(version: u32) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let temp_dir = std::env::temp_dir().join(format!("aion-reload-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&temp_dir)?;
    let source_path = temp_dir.join(format!("{RELOAD_MODULE}.erl"));
    let beam_path = temp_dir.join(format!("{RELOAD_MODULE}.beam"));
    std::fs::write(
        &source_path,
        format!(
            "-module({RELOAD_MODULE}).\n\
             -export([run/1, park/1, gated/1]).\n\
             run(_Input) -> {version}.\n\
             park(_Input) -> receive _Any -> {version} end.\n\
             gated(_Input) ->\n\
             {{ok, _Step}} = aion_flow_ffi:receive_signal(<<\"step\">>, <<\"{{}}\">>),\n\
             {{ok, _Release}} = aion_flow_ffi:receive_signal(<<\"release\">>, <<\"{{}}\">>),\n\
             {version}.\n"
        ),
    )?;
    let status = Command::new("erlc")
        .arg("-o")
        .arg(&temp_dir)
        .arg(&source_path)
        .status()?;
    if !status.success() {
        let cleanup = std::fs::remove_dir_all(&temp_dir);
        drop(cleanup);
        return Err(format!("erlc failed with status {status}").into());
    }
    let bytes = std::fs::read(beam_path)?;
    std::fs::remove_dir_all(temp_dir)?;
    Ok(bytes)
}

pub fn reload_package(
    beam: &[u8],
    entry_function: &str,
) -> Result<Package, Box<dyn std::error::Error>> {
    let beams = BeamSet::new(vec![BeamModule::new(RELOAD_MODULE, beam.to_vec())])?;
    let manifest = Manifest {
        entry_module: RELOAD_MODULE.to_owned(),
        entry_function: entry_function.to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "integer" }),
        timeout: Duration::from_secs(30),
        activities: vec![],
        version: ManifestVersion::new("test"),
        format_version: CURRENT_FORMAT_VERSION,
    };
    let archive = PackageBuilder::new(manifest, beams).write_to_bytes()?;
    Ok(Package::load_from_bytes(
        archive,
        ExtractionLimits::unbounded(),
    )?)
}

pub async fn engine_with(
    store: &Arc<dyn EventStore>,
    packages: Vec<Package>,
) -> Result<Engine, Box<dyn std::error::Error>> {
    let mut builder = EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        });
    for package in packages {
        builder = builder.load_workflows(package);
    }
    Ok(builder.build().await?)
}

pub fn input() -> Result<Payload, aion_core::PayloadError> {
    Payload::from_json(&json!({ "reload": true }))
}

pub async fn start(engine: &Engine) -> Result<(WorkflowId, RunId), Box<dyn std::error::Error>> {
    let handle = engine
        .start_workflow(
            RELOAD_MODULE,
            input()?,
            HashMap::new(),
            String::from("default"),
        )
        .await?;
    Ok((handle.workflow_id().clone(), handle.run_id().clone()))
}

pub async fn result_int(
    engine: &Engine,
    id: &WorkflowId,
    run: &RunId,
) -> Result<i64, Box<dyn std::error::Error>> {
    let payload = engine
        .result(id, run)
        .await?
        .map_err(|error| format!("workflow failed: {error:?}"))?;
    let value: serde_json::Value = serde_json::from_slice(payload.bytes())?;
    value
        .as_i64()
        .ok_or_else(|| format!("expected integer result, got {value}").into())
}

pub fn recorded_version(
    history: &[Event],
    run_id: &RunId,
) -> Result<PackageVersion, Box<dyn std::error::Error>> {
    history
        .iter()
        .find_map(|event| match event {
            Event::WorkflowStarted {
                run_id: started_run,
                package_version,
                ..
            } if started_run == run_id => Some(package_version.clone()),
            _ => None,
        })
        .ok_or_else(|| "run has no WorkflowStarted".into())
}

pub fn version_of(package: &Package) -> PackageVersion {
    PackageVersion::new(package.content_hash().to_string())
}
