//! Packages the Aion failover demo's "AI-agent-task fleet" workload into a
//! `.aion` archive an `aion server` loads via `workflow_packages`.
//!
//! The workload is the `collect_four` fan-out fixture — the proven exactly-once
//! shape. It fans four agent-ish activities (`fan:0`..`fan:3`) out through the
//! suspending `collect_all` native; each ordinal records EXACTLY ONE terminal
//! through the store-backed `record_fan_out_completion` dedup primitive, so the
//! collected result is exactly-once even when the work-owning node is killed
//! mid-flight and a survivor re-dispatches the pending ordinals.
//!
//! This binary builds the SAME archive the proven OS-process failover test
//! builds in-process (`crates/aion-cli/tests/common/osproc.rs`), so the demo and
//! the regression test exercise one workload. The fixture beam/erl are embedded
//! at compile time; swap those two inputs and the manifest entry to drop in a
//! real Norn-agent workflow later — the seam is exactly here.
//!
//! Usage: `fleet-packager <output-path.aion>`. Exits non-zero with a diagnostic
//! on any failure; never panics.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion,
    PackageBuilder,
};

/// Logical module name of the embedded fan-out fixture.
const FLEET_MODULE: &str = "aion_outbox_fixture";
/// The fan-out fixture beam (same artifact the proven failover test loads).
const FLEET_BEAM: &[u8] =
    include_bytes!("../../../crates/aion-server/tests/fixtures/aion_outbox_fixture.beam");
/// The fixture's source, carried for provenance/inspection.
const FLEET_SOURCE: &[u8] =
    include_bytes!("../../../crates/aion-server/tests/fixtures/aion_outbox_fixture.erl");
/// The workflow entry point the demo starts.
const ENTRY_FUNCTION: &str = "collect_four";
/// The activity type each fan-out ordinal dispatches; the worker registers one
/// handler per `fan:N`.
const FLEET_ACTIVITY: &str = "fixture_activity";

/// Build the demo workload archive and write it to `output`.
fn package(output: &Path) -> Result<(), String> {
    let module = BeamModule::new(FLEET_MODULE, FLEET_BEAM.to_vec());
    let beams = BeamSet::new(vec![module]).map_err(|error| format!("beam set: {error}"))?;
    let manifest = Manifest {
        entry_module: FLEET_MODULE.to_owned(),
        entry_function: ENTRY_FUNCTION.to_owned(),
        input_schema: serde_json::json!({ "type": "object" }),
        output_schema: serde_json::json!({}),
        timeout: Duration::from_secs(120),
        activities: vec![DeclaredActivity {
            activity_type: FLEET_ACTIVITY.to_owned(),
        }],
        version: ManifestVersion::new("fleet-demo"),
        format_version: CURRENT_FORMAT_VERSION,
    };
    let builder =
        PackageBuilder::with_source(manifest, beams, [(FLEET_MODULE, FLEET_SOURCE.to_vec())]);
    builder
        .write_to_path(output)
        .map_err(|error| format!("write archive {}: {error}", output.display()))
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(output) = args.next() else {
        eprintln!("usage: fleet-packager <output-path.aion>");
        return ExitCode::FAILURE;
    };
    match package(Path::new(&output)) {
        Ok(()) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("fleet-packager: {error}");
            ExitCode::FAILURE
        }
    }
}
