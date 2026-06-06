use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use aion_package::{
    BeamModule, BeamSet, DeclaredActivity, Manifest, ManifestVersion, PackageBuilder,
    CURRENT_FORMAT_VERSION,
};
use anyhow::{bail, Context, Result};
use serde_json::json;

const ENTRY_MODULE: &str = "order_saga";
const ENTRY_FUNCTION: &str = "run";
const OUTPUT: &str = "order-saga.aion";

fn main() -> Result<()> {
    let workflow_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let beams = read_compiled_beams(&workflow_root)?;
    let manifest = manifest();
    let source = [(
        ENTRY_MODULE,
        fs::read(workflow_root.join("src/order_saga.gleam"))?,
    )];
    let output_path = workflow_root.join(OUTPUT);

    PackageBuilder::with_source(manifest, beams, source).write_to_path(&output_path)?;

    println!("wrote {}", output_path.display());
    Ok(())
}

fn manifest() -> Manifest {
    Manifest {
        entry_module: ENTRY_MODULE.to_owned(),
        entry_function: ENTRY_FUNCTION.to_owned(),
        input_schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "required": ["order_id", "item", "quantity", "amount"],
            "additionalProperties": false,
            "properties": {
                "order_id": { "type": "string", "minLength": 1 },
                "item": { "type": "string", "minLength": 1 },
                "quantity": { "type": "integer", "minimum": 1 },
                "amount": { "type": "integer", "minimum": 1 }
            }
        }),
        output_schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "oneOf": [
                {
                    "type": "object",
                    "required": ["order_id", "shipment_id", "confirmation_id"],
                    "additionalProperties": false,
                    "properties": {
                        "order_id": { "type": "string" },
                        "shipment_id": { "type": "string" },
                        "confirmation_id": { "type": "string" }
                    }
                },
                {
                    "type": "object",
                    "required": ["type", "failed_step", "reason", "completed_steps", "compensations"],
                    "additionalProperties": false,
                    "properties": {
                        "type": { "const": "saga_failed" },
                        "failed_step": { "type": "string" },
                        "reason": { "type": "string" },
                        "completed_steps": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "compensations": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["step", "status", "detail"],
                                "additionalProperties": false,
                                "properties": {
                                    "step": { "type": "string" },
                                    "status": { "type": "string" },
                                    "detail": { "type": "string" }
                                }
                            }
                        }
                    }
                }
            ]
        }),
        timeout: Duration::from_secs(60),
        activities: [
            "charge_payment",
            "reserve_inventory",
            "ship_order",
            "confirm_order",
            "release_inventory",
            "refund_payment",
        ]
        .into_iter()
        .map(|activity_type| DeclaredActivity {
            activity_type: activity_type.to_owned(),
        })
        .collect(),
        version: ManifestVersion::new("unstamped"),
        format_version: CURRENT_FORMAT_VERSION,
    }
}

fn read_compiled_beams(workflow_root: &Path) -> Result<BeamSet> {
    let ebin = workflow_root.join("build/dev/erlang/aion_order_saga/_gleam_artefacts");
    if !ebin.exists() {
        bail!(
            "compiled BEAM directory {} does not exist; run `gleam build` in examples/order-saga first",
            ebin.display()
        );
    }

    let mut modules = Vec::new();
    for entry in fs::read_dir(&ebin).with_context(|| format!("reading {}", ebin.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("beam") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .context("compiled BEAM filename is not valid UTF-8")?;
        modules.push(BeamModule::new(stem, fs::read(&path)?));
    }

    if modules.iter().all(|module| module.name() != ENTRY_MODULE) {
        bail!(
            "entry module {ENTRY_MODULE}.beam was not found in {}; run `gleam build` again",
            ebin.display()
        );
    }

    BeamSet::new(modules).context("building canonical BEAM set")
}
