//! End-to-end packaging of the real hello-world example through
//! `package_project`, byte-compared against a direct `PackageBuilder`
//! construction of the same inputs (R23 determinism), then round-tripped
//! through `Package::load_from_path`.
//!
//! Requires `examples/hello-world` to be built (`gleam build`); skips at
//! runtime otherwise. The example tree is copied into a temporary project so
//! the `workflow.toml` fixture never touches `examples/`.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExcludedReason,
    ExtractionLimits, Manifest, ManifestVersion, Package, PackageBuilder, PackageOptions,
    package_project,
};
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const WORKFLOW_TOML: &str = r#"[[workflow]]
entry_module = "hello_world"
entry_function = "run"
timeout_seconds = 30
input_schema = "schemas/input.json"
output_schema = "schemas/output.json"
activities = ["greet"]
output = "hello-world.aion"
"#;

fn hello_world_example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/hello-world")
}

fn input_schema() -> serde_json::Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "required": ["name"],
        "additionalProperties": false,
        "properties": {
            "name": { "type": "string", "minLength": 1 }
        }
    })
}

fn output_schema() -> serde_json::Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "string"
    })
}

fn copy_tree(from: &Path, to: &Path) -> TestResult {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn temp_copy_of_hello_world(example: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let root = std::env::temp_dir().join(format!("aion-hello-world-{nanos}"));
    fs::create_dir_all(&root)?;
    fs::copy(example.join("gleam.toml"), root.join("gleam.toml"))?;
    fs::copy(example.join("manifest.toml"), root.join("manifest.toml"))?;
    copy_tree(&example.join("src"), &root.join("src"))?;
    copy_tree(
        &example.join("build/dev/erlang"),
        &root.join("build/dev/erlang"),
    )?;

    fs::write(root.join("workflow.toml"), WORKFLOW_TOML)?;
    fs::create_dir_all(root.join("schemas"))?;
    fs::write(
        root.join("schemas/input.json"),
        serde_json::to_vec_pretty(&input_schema())?,
    )?;
    fs::write(
        root.join("schemas/output.json"),
        serde_json::to_vec_pretty(&output_schema())?,
    )?;
    Ok(root)
}

fn is_sdk_test_only(stem: &str) -> bool {
    stem == "aion_flow_ffi" || stem == "aion@testing" || stem.starts_with("aion@testing@")
}

/// Reads every shippable compiled module exactly as the seven hand-rolled
/// packagers did, in deliberately reversed order to prove order independence.
fn read_beams_directly(root: &Path) -> Result<BeamSet, Box<dyn std::error::Error>> {
    let erlang_root = root.join("build/dev/erlang");
    let mut package_dirs = Vec::new();
    for entry in fs::read_dir(&erlang_root)? {
        package_dirs.push(entry?.path());
    }
    package_dirs.sort();
    package_dirs.reverse();

    let mut modules = Vec::new();
    for package_dir in package_dirs {
        let is_sdk_package = package_dir.file_name() == Some("aion_flow".as_ref());
        let ebin = package_dir.join("ebin");
        if !ebin.is_dir() {
            continue;
        }
        let mut paths = Vec::new();
        for entry in fs::read_dir(&ebin)? {
            paths.push(entry?.path());
        }
        paths.sort();
        paths.reverse();
        for path in paths {
            if path.extension() != Some("beam".as_ref()) {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or("non-UTF-8 beam filename")?;
            if is_sdk_package && is_sdk_test_only(stem) {
                continue;
            }
            modules.push(BeamModule::new(stem, fs::read(&path)?));
        }
    }
    Ok(BeamSet::new(modules)?)
}

fn direct_builder_archive(root: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let beams = read_beams_directly(root)?;
    let manifest = Manifest {
        entry_module: "hello_world".to_owned(),
        entry_function: "run".to_owned(),
        input_schema: input_schema(),
        output_schema: output_schema(),
        timeout: Duration::from_secs(30),
        activities: vec![DeclaredActivity {
            activity_type: "greet".to_owned(),
        }],
        version: ManifestVersion::new("unstamped"),
        format_version: CURRENT_FORMAT_VERSION,
    };
    let source = BTreeMap::from([(
        "hello_world".to_owned(),
        fs::read(root.join("src/hello_world.gleam"))?,
    )]);
    Ok(PackageBuilder::with_source(manifest, beams, source).write_to_bytes()?)
}

fn assert_report_shape(report: &aion_package::ProjectReport) {
    assert_eq!(report.packages.len(), 1);
    let packaged = &report.packages[0];
    assert_eq!(packaged.workflow_type, "hello_world");
    assert!(packaged.output_path.is_absolute());

    assert!(
        report
            .excluded
            .iter()
            .all(|excluded| excluded.reason == ExcludedReason::SdkTestOnly
                && excluded.package == "aion_flow"),
        "hello-world has no dev dependencies; only SDK exclusions expected"
    );
    assert!(
        report
            .excluded
            .iter()
            .any(|excluded| excluded.module == "aion_flow_ffi")
    );
    assert!(
        report
            .excluded
            .iter()
            .any(|excluded| excluded.module == "aion@testing")
    );
}

fn assert_round_trip(root: &Path, packaged: &aion_package::PackagedWorkflow) -> TestResult {
    let reloaded =
        Package::load_from_path(root.join("hello-world.aion"), ExtractionLimits::unbounded())?;
    assert_eq!(&reloaded, &packaged.package);

    let record = reloaded.version_record();
    assert_eq!(record, packaged.version);
    assert_eq!(record.entry_module, "hello_world");
    assert_eq!(record.activities.len(), 1);
    assert_eq!(record.activities[0].activity_type, "greet");
    assert_eq!(record.input_schema, input_schema());
    assert_eq!(record.output_schema, output_schema());

    let manifest = reloaded.manifest();
    assert_eq!(manifest.entry_function, "run");
    assert_eq!(manifest.timeout, Duration::from_secs(30));
    assert_eq!(manifest.version.as_str(), record.content_hash.to_string());
    assert!(reloaded.deployed_entry_module().starts_with("hello_world$"));
    assert!(reloaded.beams().get("hello_world").is_some());
    assert!(reloaded.beams().get("aion_flow_ffi").is_none());
    assert!(reloaded.source().contains_key("hello_world"));
    Ok(())
}

#[test]
fn packages_hello_world_byte_identically_to_direct_construction() -> TestResult {
    let example = hello_world_example();
    if !example.join("build/dev/erlang").is_dir() {
        println!(
            "skipping: {} is not built; run `gleam build` in examples/hello-world",
            example.display()
        );
        return Ok(());
    }

    let root = temp_copy_of_hello_world(&example)?;
    let report = package_project(&root, &PackageOptions::default());
    let library_bytes = fs::read(root.join("hello-world.aion"));
    let direct_bytes = direct_builder_archive(&root);
    let round_trip = report.as_ref().ok().map(assert_report_shape);
    let reload_check = report
        .as_ref()
        .ok()
        .map(|report| assert_round_trip(&root, &report.packages[0]));
    fs::remove_dir_all(&root)?;

    report?;
    round_trip.ok_or("packaging failed before report checks")?;
    reload_check.ok_or("packaging failed before reload checks")??;
    let library_bytes = library_bytes?;
    assert!(!library_bytes.is_empty());
    assert_eq!(
        library_bytes, direct_bytes?,
        "library archive differs from direct PackageBuilder construction"
    );
    Ok(())
}
