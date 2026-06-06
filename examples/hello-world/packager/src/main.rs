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

const ENTRY_MODULE: &str = "hello_world";
const ENTRY_FUNCTION: &str = "run";
const OUTPUT: &str = "hello-world.aion";

fn main() -> Result<()> {
    let workflow_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let beams = read_compiled_beams(&workflow_root)?;
    let manifest = manifest();
    let source = [(
        ENTRY_MODULE,
        fs::read(workflow_root.join("src/hello_world.gleam"))?,
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
            "required": ["name"],
            "additionalProperties": false,
            "properties": {
                "name": { "type": "string", "minLength": 1 }
            }
        }),
        output_schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "string"
        }),
        timeout: Duration::from_secs(30),
        activities: vec![DeclaredActivity {
            activity_type: "greet".to_owned(),
        }],
        version: ManifestVersion::new("unstamped"),
        format_version: CURRENT_FORMAT_VERSION,
    }
}

fn read_compiled_beams(workflow_root: &Path) -> Result<BeamSet> {
    let ebin = workflow_root.join("build/dev/erlang/aion_hello_world/ebin");
    if !ebin.exists() {
        bail!(
            "compiled BEAM directory {} does not exist; run `gleam build` in examples/hello-world first",
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
