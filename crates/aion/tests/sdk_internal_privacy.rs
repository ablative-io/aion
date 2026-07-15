//! External-package compile probes for the SDK's activity-dispatch boundary.

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> Result<PathBuf, Box<dyn Error>> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "failed to resolve repository root".into())
}

fn reset(path: &Path) -> Result<(), Box<dyn Error>> {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    fs::create_dir_all(path.join("src"))?;
    Ok(())
}

fn write_project(
    path: &Path,
    sdk: &Path,
    module: &str,
    source: &str,
) -> Result<(), Box<dyn Error>> {
    let sdk = sdk
        .to_str()
        .ok_or("SDK path is not valid UTF-8")?
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    fs::write(
        path.join("gleam.toml"),
        format!(
            "name = \"{module}\"\nversion = \"1.0.0\"\ntarget = \"erlang\"\n\n\
             [dependencies]\naion_flow = {{ path = \"{sdk}\" }}\n\
             gleam_stdlib = \">= 0.44.0 and < 2.0.0\"\n\
             gleam_json = \">= 3.0.0 and < 4.0.0\"\n"
        ),
    )?;
    fs::write(path.join("src").join(format!("{module}.gleam")), source)?;
    Ok(())
}

fn gleam_build(path: &Path) -> Result<Output, Box<dyn Error>> {
    Ok(Command::new("gleam")
        .arg("build")
        .current_dir(path)
        .output()?)
}

#[test]
fn external_package_cannot_dispatch_without_await_but_public_run_compiles()
-> Result<(), Box<dyn Error>> {
    let root = repo_root()?;
    let sdk = root.join("gleam/aion_flow");
    let probes = root.join("target/fix3-sdk-boundary-probes");
    let negative = probes.join("negative");
    let positive = probes.join("positive");
    reset(&negative)?;
    reset(&positive)?;

    write_project(
        &negative,
        &sdk,
        "dispatch_negative",
        r#"import aion/activity
import aion/codec
import aion/error
import aion/internal/activity_dispatch
import gleam/dynamic/decode
import gleam/json
import gleam/option.{None}

pub fn dispatch_without_await() {
  let wire = codec.json_codec(json.string, decode.string)
  let invocation = activity.new("probe", "input", wire, wire, fn(_) {
    Error(error.terminal("unused"))
  })
  activity_dispatch.dispatch(invocation, None)
}
"#,
    )?;
    let denied = gleam_build(&negative)?;
    if denied.status.success() {
        return Err("external package unexpectedly called raw activity dispatch".into());
    }
    let denied_stderr = String::from_utf8_lossy(&denied.stderr);
    if !denied_stderr.contains("does not have a `dispatch`") {
        return Err(format!("negative probe failed for the wrong reason:\n{denied_stderr}").into());
    }

    write_project(
        &positive,
        &sdk,
        "workflow_run_positive",
        r#"import aion/activity
import aion/codec
import aion/workflow
import gleam/dynamic/decode
import gleam/json

pub fn run_public_activity() {
  let wire = codec.json_codec(json.string, decode.string)
  let invocation = activity.new("probe", "input", wire, wire, fn(input) {
    Ok(input)
  })
  workflow.run(invocation)
}
"#,
    )?;
    let allowed = gleam_build(&positive)?;
    if !allowed.status.success() {
        return Err(format!(
            "public workflow.run probe did not compile:\n{}",
            String::from_utf8_lossy(&allowed.stderr)
        )
        .into());
    }
    Ok(())
}
