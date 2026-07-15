//! Real `gleam build` compile proofs for the B4 flow-vocabulary lowering:
//! every flow-shape fixture (regions, subflows, `max … visits`, tolerant
//! and strict collects, value route payloads, the retired `on failure`
//! body-terminal-route refusal) plus the two fixtures B2 regressed to
//! refused (`backward_route_bounded_cycle`, `ship_release_combined`) —
//! each checks, emits, and compiles clean under the real Gleam toolchain,
//! all in one generated project. The generated project lives under the
//! workspace `target/` directory (never the system temp dir).

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use aion_awl::{check_in, emit_in, parse};

type TestResult = Result<(), Box<dyn Error>>;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> Result<PathBuf, Box<dyn Error>> {
    crate_root()
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "failed to resolve repository root from CARGO_MANIFEST_DIR".into())
}

/// Every fixture the B4 landing proves end to end: relative path and the
/// generated Gleam module name (the workflow name).
const PROVEN: &[(&str, &str)] = &[
    ("flow-shape/valid/dev_flow.awl", "dev_flow"),
    ("flow-shape/valid/sequence_region_loopback.awl", "rollout"),
    (
        "flow-shape/valid/distribute_activity_tolerant.awl",
        "fan_scores",
    ),
    (
        "flow-shape/valid/distribute_child_collect.awl",
        "fan_children",
    ),
    (
        "flow-shape/valid/distribute_child_tolerant.awl",
        "fan_children_tolerant",
    ),
    (
        "flow-shape/valid/sequence_activity_tolerant.awl",
        "ordered_tolerant",
    ),
    ("flow-shape/valid/subflow_nested.awl", "nested_subflows"),
    ("flow-shape/valid/subflow_local_names.awl", "scoped_names"),
    (
        "flow-shape/valid/region_pure_decision.awl",
        "region_decision",
    ),
    ("flow-shape/valid/counter_hygiene.awl", "counter_hygiene"),
    (
        "flow-shape/valid/on_failure_route_tail.awl",
        "publish_with_cleanup",
    ),
    (
        "flow-shape/valid/substep_on_failure_route_tail.awl",
        "substep_cleanup",
    ),
    ("flow-shape/valid/value_route_payload.awl", "value_route"),
    (
        "loop-outcomes/valid/backward_route_bounded_cycle.awl",
        "drafting",
    ),
    (
        "loop-outcomes/valid/ship_release_combined.awl",
        "ship_release",
    ),
];

/// The B4 landing bar: every proven fixture checks, emits, and the whole
/// set `gleam build`s clean as one project.
#[test]
fn b4_flow_shape_fixtures_compile_under_gleam() -> TestResult {
    let fixtures_root = crate_root().join("tests/fixtures/rev2");
    let repo_root = repo_root()?;
    let project = repo_root.join("target/flow-vocab-b4-proof");
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("gleam.toml"),
        format!(
            "name = \"awl_flow_vocab_b4_proof\"\nversion = \"0.1.0\"\ntarget = \
             \"erlang\"\n\n[dependencies]\naion_flow = {{ path = \"{}\" }}\ngleam_stdlib = \
             \">= 0.34.0 and < 2.0.0\"\ngleam_json = \">= 2.0.0 and < 4.0.0\"\n",
            repo_root.join("gleam/aion_flow").display()
        ),
    )?;
    for (relative, module) in PROVEN {
        let fixture = fixtures_root.join(relative);
        let source = fs::read_to_string(&fixture)?;
        let document = parse(&source).map_err(|error| format!("{relative}: {error}"))?;
        let fixture_dir = fixture
            .parent()
            .ok_or("fixture path has no parent directory")?;
        let errors = check_in(&document, fixture_dir);
        assert!(errors.is_empty(), "{relative} must check clean: {errors:?}");
        let generated = emit_in(&document, fixture_dir)
            .map_err(|error| format!("{relative}: emit refused: {}", error.message))?;
        fs::write(
            project.join("src").join(format!("{module}.gleam")),
            generated,
        )?;
    }

    let output = Command::new("gleam")
        .arg("build")
        .current_dir(&project)
        .output()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("gleam binary is required for AWL emitter compile-proof tests: {error}"),
            )
        })?;
    assert!(
        output.status.success(),
        "gleam build failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
