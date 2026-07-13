//! Real Gleam compiler proofs for representative emitted modules.

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{emitted_archived_exam, emitted_fixture};

/// Flagship compile proof: `awl_hello` and `dev_brief` build under real
/// `gleam build` against the local SDK (#248: if this fails under parallel
/// load, re-run isolated once before treating it as red).
#[test]
fn flagship_fixtures_compile_under_gleam() -> Result<(), Box<dyn Error>> {
    compile_generated_modules(
        "flagship",
        &[
            (
                "awl_hello",
                emitted_fixture("flagship/valid/awl_hello.awl")?,
            ),
            (
                "dev_brief",
                emitted_fixture("flagship/valid/dev_brief.awl")?,
            ),
        ],
    )
}

/// Loop/fork-heavy compile proof: the combined fixtures exercising loops,
/// counters, forks in all three forms, waits, routes, and compensation all
/// build under real `gleam build`.
#[test]
fn loop_and_fork_fixtures_compile_under_gleam() -> Result<(), Box<dyn Error>> {
    compile_generated_modules(
        "loop_fork",
        &[
            (
                "ship_release",
                emitted_fixture("loop-outcomes/valid/ship_release_combined.awl")?,
            ),
            (
                "release_pipeline",
                emitted_fixture("dag-fork/valid/release_pipeline_combined.awl")?,
            ),
            (
                "fork_named",
                emitted_fixture("dag-fork/valid/fork_named_branches.awl")?,
            ),
            (
                "child_collection_fork",
                emitted_fixture("dag-fork/valid/child_collection_fork.awl")?,
            ),
            ("archived_awl_exam", emitted_archived_exam()?),
            (
                "backward_route",
                emitted_fixture("loop-outcomes/valid/backward_route_bounded_cycle.awl")?,
            ),
            (
                "compound_until_nested",
                emitted_fixture("loop-outcomes/valid/loop_compound_until_nested.awl")?,
            ),
            (
                "loop_without_counting",
                emitted_fixture("loop-outcomes/valid/loop_without_counting.awl")?,
            ),
            (
                "substeps",
                emitted_fixture("loop-outcomes/valid/substeps_two_stage.awl")?,
            ),
            (
                "combinators",
                emitted_fixture("step-bodies/valid/combinators.awl")?,
            ),
            (
                "float_guard",
                emitted_fixture("loop-outcomes/valid/float_threshold_guard.awl")?,
            ),
            (
                "step_bodies",
                emitted_fixture("step-bodies/valid/step_bodies_combined.awl")?,
            ),
            (
                "mixed_doors",
                emitted_fixture("schema-doors/valid/mixed_doors.awl")?,
            ),
            (
                "declarations",
                emitted_fixture("declarations/valid/declarations_combined.awl")?,
            ),
        ],
    )
}

fn compile_generated_modules(
    project_name: &str,
    modules: &[(&str, String)],
) -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root.parent().and_then(Path::parent).ok_or_else(|| {
        io::Error::other("failed to resolve repository root from CARGO_MANIFEST_DIR")
    })?;
    let project = unique_temp_project(project_name);
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("gleam.toml"),
        format!(
            "name = \"awl_{project_name}_compile_proof\"\nversion = \"0.1.0\"\ntarget = \
             \"erlang\"\n\n[dependencies]\naion_flow = {{ path = \"{}\" }}\ngleam_stdlib = \
             \">= 0.34.0 and < 2.0.0\"\ngleam_json = \">= 2.0.0 and < 4.0.0\"\n",
            repo_root.join("gleam/aion_flow").display()
        ),
    )?;
    for (module_name, module_source) in modules {
        fs::write(
            project.join("src").join(format!("{module_name}.gleam")),
            module_source,
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

fn unique_temp_project(project_name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "aion_awl_{project_name}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));
    path
}
