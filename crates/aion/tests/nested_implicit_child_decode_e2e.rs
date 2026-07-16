//! Exact nested implicit-child codec and await-boundary regression.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion::signal::ConcreteSignalRouter;
use aion::{EngineBuilder, RuntimeHandle, SignalRouter};
use aion_core::{Event, Payload, WorkflowError};
use aion_package::{PackageOptions, package_project};
use aion_store::{EventStore, InMemoryStore};
use serde_json::{Value, json};

const MODULE: &str = "nested_child_decode";
const DURABLE_OUTER_RESULT: &[u8] = br#"{"outcome":"child","payload":2}"#;
const AWL: &str = r"
//! Nested implicit-child decode boundary proof.
workflow nested_child_decode
  input groups: [[Int]]
  input repeat_limit: Int
  outcome done: type Int, route success

worker proof
  action delay(value: Int) -> Int

subflow bounded(value: Int, repeat_limit: Int)
  outcome out: type Int

  step pause
    delay(value: value) -> delayed

  step again after pause
    outcome repeat: when visits < repeat_limit, route again
    outcome finish: otherwise, route out(delayed)
    max 2 visits

subflow nested(value: Int, repeat_limit: Int)
  outcome out: type Int

  step call
    bounded(value: value, repeat_limit: repeat_limit) -> cycled

  step return
    cycled |> route out

step outer_wave
  distribute group in groups

step inner_wave
  distribute item in group

step member
  nested(value: item, repeat_limit: repeat_limit) -> result

step inner_gather
  collect result -> results
  results |> count -> gathered

step outer_gather
  collect gathered -> totals
  totals |> count -> group_count
  route done(group_count)
";

struct DelayedIntDispatcher;

impl ActivityDispatcher for DelayedIntDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        if request.name != "delay" {
            return Err(format!("unexpected activity {}", request.name));
        }
        std::thread::sleep(Duration::from_millis(200));
        let input: Value =
            serde_json::from_str(&request.input).map_err(|error| error.to_string())?;
        serde_json::to_string(
            input
                .get("value")
                .ok_or_else(|| "delay input has no value".to_owned())?,
        )
        .map_err(|error| error.to_string())
    }
}

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn repo_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot resolve repository root".into())
}

fn run_checked(command: &mut Command, description: &str) -> TestResult {
    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{description} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

fn render_failure(error: &WorkflowError) -> String {
    let details = error.details.as_ref().map_or_else(
        || "<none>".to_owned(),
        |payload| String::from_utf8_lossy(payload.bytes()).into_owned(),
    );
    format!("{error:?}; durable details={details}")
}

fn write_and_check_nested_project(repo: &Path, root: &Path) -> TestResult {
    fs::create_dir_all(root.join("src"))?;
    fs::create_dir_all(root.join("schemas"))?;
    let document = aion_awl::parse(AWL)?;
    let diagnostics = aion_awl::check(&document);
    if !diagnostics.is_empty() {
        return Err(format!("nested decode AWL did not check: {diagnostics:?}").into());
    }
    let artifact = aion_awl::emit_artifact(&document)?;
    assert_eq!(artifact.synthesized_workflows.len(), 2);
    assert!(artifact.source.contains("repeat_limit"));

    let generated = format!(
        "{}\n\npub fn awl_test_decode_exact_child_payload(raw: String) -> Result(Int, codec.DecodeError) {{\n  let child_codec = awl_child_output_int_codec()\n  child_codec.decode(raw)\n}}\n",
        artifact.source
    );
    fs::write(root.join("src").join(format!("{MODULE}.gleam")), generated)?;
    fs::write(
        root.join("src/codec_probe.gleam"),
        "import nested_child_decode\n\npub fn main() {\n  let assert Ok(2) = nested_child_decode.awl_test_decode_exact_child_payload(\"{\\\"outcome\\\":\\\"child\\\",\\\"payload\\\":2}\")\n  Nil\n}\n",
    )?;
    fs::write(
        root.join("src").join(format!("{MODULE}.awl.json")),
        serde_json::to_vec_pretty(&artifact.project_metadata())?,
    )?;
    fs::write(
        root.join("schemas/input.json"),
        serde_json::to_vec_pretty(&aion_awl::schema_for_workflow(&document)?)?,
    )?;
    fs::write(
        root.join("schemas/output.json"),
        serde_json::to_vec_pretty(&aion_awl::schema_for_outcomes(&document)?)?,
    )?;
    fs::write(
        root.join("workflow.toml"),
        format!(
            "[[workflow]]\nentry_module = \"{MODULE}\"\nentry_function = \"run\"\ntimeout_seconds = 30\ninput_schema = \"schemas/input.json\"\noutput_schema = \"schemas/output.json\"\nactivities = [\"delay\"]\n"
        ),
    )?;
    fs::write(
        root.join("gleam.toml"),
        format!(
            "name = \"fix2_nested_decode\"\nversion = \"1.0.0\"\ntarget = \"erlang\"\n\n[dependencies]\ngleam_stdlib = \">= 0.44.0 and < 3.0.0\"\ngleam_json = \">= 3.0.0 and < 4.0.0\"\naion_flow = {{ path = \"{}\" }}\n",
            repo.join("gleam/aion_flow").display()
        ),
    )?;

    run_checked(
        Command::new("gleam")
            .args(["run", "-m", "codec_probe"])
            .current_dir(root),
        "generated child codec exact-byte probe",
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn nested_outer_int_child_decodes_exact_durable_bytes_and_parent_completes() -> TestResult {
    let repo = repo_root()?;
    let root = repo.join("target/flow-vocab-b5-fix2-nested-decode");
    write_and_check_nested_project(&repo, &root)?;
    let mut report = package_project(&root, &PackageOptions::default())?;
    let packaged = report
        .packages
        .pop()
        .ok_or("project packaging produced no archive")?;
    assert!(report.packages.is_empty());
    assert_eq!(packaged.package.manifest().additional_workflows.len(), 2);

    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(4)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .activity_dispatcher(Arc::new(DelayedIntDispatcher))
        .load_workflows(packaged.package)
        .build()
        .await?;
    let handle = engine
        .start_workflow(
            MODULE,
            Payload::from_json(&json!({ "groups": [[1, 2], [3]], "repeat_limit": 2 }))?,
            std::collections::HashMap::new(),
            "default".to_owned(),
        )
        .await?;
    let workflow_id = handle.workflow_id().clone();
    let run_id = handle.run_id().clone();
    let terminal = tokio::time::timeout(
        Duration::from_secs(20),
        engine.result(&workflow_id, &run_id),
    )
    .await??;
    let result = terminal.map_err(|error| {
        format!(
            "nested parent failed; concrete child boundary evidence: {}",
            render_failure(&error)
        )
    })?;
    let decoded: Value = serde_json::from_slice(result.bytes())?;
    assert_eq!(decoded["outcome"], "done");
    assert_eq!(decoded["payload"], 2);

    let parent_history = store.read_history(&workflow_id).await?;
    assert!(parent_history.iter().any(|event| {
        matches!(event, Event::ChildWorkflowCompleted { result, .. } if result.bytes() == DURABLE_OUTER_RESULT)
    }));
    engine.shutdown()?;
    Ok(())
}
