//! Shared helpers for aion engine integration tests.

use std::sync::Arc;
use std::time::Duration;

use aion::{Engine, EngineBuilder};
use aion_core::Payload;
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion,
    Package, PackageBuilder,
};
use aion_store::{EventStore, InMemoryStore};
use serde_json::json;

pub const FIXTURE_MODULE: &str = "aion_fixture_workflow";
const FIXTURE_BEAM: &[u8] = include_bytes!("../fixtures/aion_fixture_workflow.beam");
const FIXTURE_SOURCE: &[u8] = include_bytes!("../fixtures/aion_fixture_workflow.erl");

pub fn payload(value: serde_json::Value) -> Result<Payload, aion_core::PayloadError> {
    Payload::from_json(&value)
}

pub fn input_payload() -> Result<Payload, aion_core::PayloadError> {
    payload(json!({ "fixture": "input" }))
}

pub fn fixture_package(entry_function: &str) -> Result<Package, Box<dyn std::error::Error>> {
    let beams = BeamSet::new(vec![BeamModule::new(FIXTURE_MODULE, FIXTURE_BEAM)])?;
    let manifest = Manifest {
        entry_module: FIXTURE_MODULE.to_owned(),
        entry_function: entry_function.to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({}),
        timeout: Duration::from_secs(30),
        activities: vec![DeclaredActivity {
            activity_type: "fixture_activity".to_owned(),
        }],
        version: ManifestVersion::new("stamped-by-builder"),
        format_version: CURRENT_FORMAT_VERSION,
    };
    let archive =
        PackageBuilder::with_source(manifest, beams, [(FIXTURE_MODULE, FIXTURE_SOURCE.to_vec())])
            .write_to_bytes()?;
    Ok(Package::load_from_bytes(archive)?)
}

pub async fn engine_with_fixture(
    entry_function: &str,
) -> Result<(Engine, Arc<dyn EventStore>), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .scheduler_threads(1)
        .load_workflows(fixture_package(entry_function)?)
        .build()
        .await?;
    Ok((engine, store))
}
