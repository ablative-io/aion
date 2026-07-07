use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use aion_awl::{emit, parse};

fn emitted(source: &str) -> Result<String, Box<dyn Error>> {
    Ok(emit(&parse(source)?))
}

fn assert_golden(source: &str, golden: &str) -> Result<(), Box<dyn Error>> {
    assert_eq!(emitted(source)?, golden);
    Ok(())
}

#[test]
fn emits_research_report_golden() -> Result<(), Box<dyn Error>> {
    assert_golden(
        include_str!("fixtures/research_report.awl"),
        include_str!("fixtures/research_report.gleam.golden"),
    )
}

#[test]
fn emits_hello_golden() -> Result<(), Box<dyn Error>> {
    assert_golden(
        include_str!("fixtures/hello.awl"),
        include_str!("fixtures/hello.gleam.golden"),
    )
}

#[test]
fn emitted_sources_do_not_call_direct_nondeterministic_runtime_apis() -> Result<(), Box<dyn Error>> {
    let emitted_sources = [
        emitted(include_str!("fixtures/hello.awl"))?,
        emitted(include_str!("fixtures/research_report.awl"))?,
    ];
    let denylist = ["erlang/", "os.", "io.", "random.", "calendar."];
    for source in emitted_sources {
        for denied in denylist {
            assert!(
                !source.contains(denied),
                "generated source contains forbidden direct runtime API {denied}"
            );
        }
    }
    Ok(())
}

#[test]
fn emitted_hello_compiles_against_local_aion_flow() -> Result<(), Box<dyn Error>> {
    compile_generated_module("hello", &emitted(include_str!("fixtures/hello.awl"))?)
}

#[test]
fn emitted_research_report_compiles_against_local_aion_flow() -> Result<(), Box<dyn Error>> {
    compile_generated_module(
        "research_report",
        &emitted(include_str!("fixtures/research_report.awl"))?,
    )
}

fn compile_generated_module(module_name: &str, module_source: &str) -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            io::Error::other("failed to resolve repository root from CARGO_MANIFEST_DIR")
        })?;
    let project = unique_temp_project(module_name);
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("gleam.toml"),
        format!(
            "name = \"awl_{module_name}_compile_proof\"\nversion = \"0.1.0\"\ntarget = \"erlang\"\n\n[dependencies]\naion_flow = {{ path = \"{}\" }}\ngleam_stdlib = \">= 0.34.0 and < 2.0.0\"\ngleam_json = \">= 2.0.0 and < 4.0.0\"\n",
            repo_root.join("gleam/aion_flow").display()
        ),
    )?;
    fs::write(project.join("src").join(format!("{module_name}.gleam")), module_source)?;

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

fn unique_temp_project(module_name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "aion_awl_{module_name}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));
    path
}
