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

const ENTRY_MODULE: &str = "approval_gate";
const ENTRY_FUNCTION: &str = "run";
const OUTPUT: &str = "approval-gate.aion";

fn main() -> Result<()> {
    let workflow_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let beams = read_compiled_beams(&workflow_root)?;
    let manifest = manifest();
    let source = [(
        ENTRY_MODULE,
        fs::read(workflow_root.join("src/approval_gate.gleam"))?,
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
            "required": ["document_id", "timeout_minutes"],
            "additionalProperties": false,
            "properties": {
                "document_id": { "type": "string", "minLength": 1 },
                "timeout_minutes": { "type": "integer", "minimum": 1 }
            }
        }),
        output_schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "required": ["decision", "action_taken", "reason"],
            "additionalProperties": false,
            "properties": {
                "decision": { "type": "string" },
                "action_taken": { "type": "string" },
                "reason": { "type": "string" }
            }
        }),
        timeout: Duration::from_secs(3600),
        activities: vec![
            DeclaredActivity {
                activity_type: "archive_document".to_owned(),
            },
            DeclaredActivity {
                activity_type: "publish_document".to_owned(),
            },
        ],
        version: ManifestVersion::new("unstamped"),
        format_version: CURRENT_FORMAT_VERSION,
    }
}

fn read_compiled_beams(workflow_root: &Path) -> Result<BeamSet> {
    let erlang_root = workflow_root.join("build/dev/erlang");
    if !erlang_root.exists() {
        bail!(
            "compiled Erlang directory {} does not exist; run `gleam build` in examples/approval-gate first",
            erlang_root.display()
        );
    }

    let mut modules = Vec::new();
    for pkg_entry in fs::read_dir(&erlang_root)
        .with_context(|| format!("reading {}", erlang_root.display()))?
    {
        let ebin = pkg_entry?.path().join("ebin");
        if !ebin.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&ebin)
            .with_context(|| format!("reading {}", ebin.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("beam") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .context("compiled BEAM filename is not valid UTF-8")?;
            if is_test_only_module(stem) {
                println!("excluding test-only module {stem}");
                continue;
            }
            modules.push(BeamModule::new(stem, fs::read(&path)?));
        }
    }

    if modules.iter().all(|module| module.name() != ENTRY_MODULE) {
        bail!(
            "entry module {ENTRY_MODULE}.beam was not found under {}; run `gleam build` again",
            erlang_root.display()
        );
    }

    BeamSet::new(modules).context("building canonical BEAM set")
}

/// Test machinery that must never ship in a workflow package.
///
/// `aion_flow_ffi` is the SDK's in-process engine double occupying the
/// engine-owned NIF namespace (also rejected by `BeamSet::new`), and the
/// `aion/testing` modules only exist to drive it from SDK unit tests.
fn is_test_only_module(stem: &str) -> bool {
    stem == "aion_flow_ffi" || stem == "aion@testing" || stem.starts_with("aion@testing@")
}
