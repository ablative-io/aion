//! Determinism proof for project packaging (T-det-2): the library's archive is
//! byte-identical to one constructed directly through `PackageBuilder` from the
//! same inputs supplied in a deliberately shuffled order.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, PackageBuilder, PackageOptions, package_project,
};
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const GLEAM_TOML: &str = r#"name = "demo"
version = "0.1.0"
target = "erlang"

[dependencies]
dep_a = ">= 1.0.0"
"#;

const LOCKFILE: &str = r#"packages = [
  { name = "dep_a", version = "1.0.0", requirements = [] },
]

[requirements]
dep_a = { version = ">= 1.0.0" }
"#;

const WORKFLOW_TOML: &str = r#"[[workflow]]
entry_module = "demo"
entry_function = "run"
timeout_seconds = 30
input_schema = "schemas/input.json"
output_schema = "schemas/output.json"
activities = ["greet"]
"#;

fn synthetic_project() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let root = std::env::temp_dir().join(format!("aion-project-determinism-{nanos}"));
    let files: [(&str, &[u8]); 9] = [
        ("gleam.toml", GLEAM_TOML.as_bytes()),
        ("manifest.toml", LOCKFILE.as_bytes()),
        ("workflow.toml", WORKFLOW_TOML.as_bytes()),
        ("schemas/input.json", br#"{ "type": "object" }"#),
        ("schemas/output.json", br#"{ "type": "string" }"#),
        ("src/demo.gleam", b"pub fn run() { Nil }"),
        ("src/demo/nested.gleam", b"pub fn helper() { Nil }"),
        ("build/dev/erlang/demo/ebin/demo.beam", b"demo-bytes"),
        ("build/dev/erlang/dep_a/ebin/dep_a.beam", b"dep-a-bytes"),
    ];
    for (relative, bytes) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, bytes)?;
    }
    Ok(root)
}

fn direct_builder_archive(root: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    // Beams hand-read in deliberately shuffled (reverse) order; `BeamSet`
    // canonicalises, so the library result must still match byte for byte.
    let beams = BeamSet::new(vec![
        BeamModule::new(
            "dep_a",
            fs::read(root.join("build/dev/erlang/dep_a/ebin/dep_a.beam"))?,
        ),
        BeamModule::new(
            "demo",
            fs::read(root.join("build/dev/erlang/demo/ebin/demo.beam"))?,
        ),
    ])?;
    let manifest = Manifest {
        entry_module: "demo".to_owned(),
        entry_function: "run".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "string" }),
        timeout: Duration::from_secs(30),
        activities: vec![DeclaredActivity {
            activity_type: "greet".to_owned(),
        }],
        version: ManifestVersion::new("unstamped"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    };
    let source = BTreeMap::from([
        (
            "demo/nested".to_owned(),
            fs::read(root.join("src/demo/nested.gleam"))?,
        ),
        ("demo".to_owned(), fs::read(root.join("src/demo.gleam"))?),
    ]);

    Ok(PackageBuilder::with_source(manifest, beams, source).write_to_bytes()?)
}

#[test]
fn library_archive_is_byte_identical_to_direct_builder_construction() -> TestResult {
    let root = synthetic_project()?;
    let report = package_project(&root, &PackageOptions::default());
    let library_bytes = fs::read(root.join("demo.aion"));
    let direct_bytes = direct_builder_archive(&root);
    fs::remove_dir_all(&root)?;

    let report = report?;
    let library_bytes = library_bytes?;
    let direct_bytes = direct_bytes?;
    assert!(!library_bytes.is_empty());
    assert_eq!(library_bytes, direct_bytes);

    let direct_package =
        aion_package::Package::load_from_bytes(&direct_bytes, ExtractionLimits::unbounded())?;
    assert_eq!(report.packages[0].version, direct_package.version_record());
    Ok(())
}
