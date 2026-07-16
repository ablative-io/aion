//! Failure-detail parity between the DIRECT bytecode path and the GLEAM path
//! on real engines — the adversarial panel's three child-failure scenarios:
//!
//! 1. **strict abort**: a child's activity fails and the strict collect
//!    aborts the parent with the child's typed error;
//! 2. **visits exhaustion**: a bounded in-region loop exhausts `max …
//!    visits` inside the implicit child;
//! 3. **in-child spawn refusal**: a `spawn` inside the region names a
//!    workflow type the archive does not carry, so the engine refuses the
//!    child's detached spawn.
//!
//! For each scenario BOTH archives run the same document to the parent's
//! terminal failure and the recorded strings must CONVERGE: the parent's
//! failure details and the durable child-failure details are byte-equal
//! JSON across paths, and the typed kind (`AwlActivityFailed`,
//! `AwlVisitsExceeded`, `AwlChildFailed`) survives the parent-child
//! boundary — the regression class where the run shell leaked the raw error
//! record and every typed kind collapsed into
//! `ChildErrorDecodeFailed: "Expected Dict, found List"` at the parent.
//!
//! KNOWN LIMIT (documented, not fixed here): implicit-child rows surface
//! `parent: null` in the workflow visibility listing. The visibility wire
//! contract carries no parent workflow id (`aion-client` refuses parent
//! filters for the same reason), so threading the spawning parent through
//! is a wire-contract extension, not a cheap change at the child-spawn
//! seam.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion::signal::ConcreteSignalRouter;
use aion::{EngineBuilder, RuntimeHandle, SignalRouter};
use aion_awl_package::compile_and_assemble_awl;
use aion_core::{Event, Payload, WorkflowError};
use aion_package::{ExtractionLimits, Package, PackageOptions, package_project};
use aion_store::{EventStore, InMemoryStore};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type TestError = Box<dyn std::error::Error>;

const STRICT_ABORT_MODULE: &str = "parity_strict_abort";
const STRICT_ABORT: &str = r#"//! Strict abort: the middle stage of an implicit child fails.
workflow parity_strict_abort
  input items: [String]
  outcome done: type Done, route success

type Done { total: Int }

worker proof
  action stage_one(item: String) -> String
  action stage_two(item: String) -> String

step fan
  distribute item in items

step first
  stage_one(item: item) -> prepared

step second
  stage_two(item: prepared) -> result

step gather
  collect result -> results
  results |> count -> total
  route done(total: total)
"#;

const VISITS_MODULE: &str = "parity_visits";
const VISITS: &str = r#"//! Visits exhaustion inside the implicit child's bounded loop.
workflow parity_visits
  input items: [String]
  outcome done: type Done, route success

type Done    { total: Int }
type Attempt { ok: Bool }

worker proof
  action deploy(item: String) -> Attempt
  action verify(item: String) -> Attempt

step fan
  distribute item in items

step push after fan
  deploy(item: item) -> attempt

step confirm
  verify(item: item) -> checked

  outcome retry: when not checked.ok and visits < 5, route push
  outcome move_on: otherwise, route settle
  max 2 visits

step settle
  collect checked -> results
  results |> count -> total
  route done(total: total)
"#;

const SPAWN_REFUSAL_MODULE: &str = "parity_spawn_refusal";
const SPAWN_REFUSAL: &str = r#"//! In-child spawn refusal: the spawned type is not in the archive.
workflow parity_spawn_refusal
  input items: [String]
  outcome done: type Done, route success

type Done { total: Int }

worker proof
  action stage_one(item: String) -> String

child audit_trail(item: String) -> Nil

step fan
  distribute item in items

step first
  spawn audit_trail(item: item)
  stage_one(item: item) -> result

step gather
  collect result -> results
  results |> count -> total
  route done(total: total)
"#;

/// One dispatcher serves every scenario (action names are disjoint):
/// `stage_two` fails terminally, `verify` never approves, the rest succeed.
struct ScenarioDispatcher;

impl ActivityDispatcher for ScenarioDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        let value: Value =
            serde_json::from_str(&request.input).map_err(|error| error.to_string())?;
        let item = value
            .get("item")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("activity {} input has no string item", request.name))?;
        match request.name.as_str() {
            "stage_one" => {
                serde_json::to_string(&format!("{item}-one")).map_err(|error| error.to_string())
            }
            "stage_two" => Err("intentional".to_owned()),
            "deploy" => Ok(json!({ "ok": true }).to_string()),
            "verify" => Ok(json!({ "ok": false }).to_string()),
            other => Err(format!("unknown parity activity {other}")),
        }
    }
}

/// A run's converged failure record: the parent's terminal failure details
/// and the durable child-failure details from the parent history.
struct FailureRecord {
    parent_details: Value,
    child_details: Value,
}

fn details_json(error: &WorkflowError) -> Result<Value, TestError> {
    let details = error
        .details
        .as_ref()
        .ok_or_else(|| format!("failure carries no details: {}", error.message))?;
    Ok(serde_json::from_slice(details.bytes())?)
}

async fn run_to_failure(package: Package, module: &str) -> Result<FailureRecord, TestError> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(2)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .activity_dispatcher(Arc::new(ScenarioDispatcher))
        .load_workflows(package)
        .build()
        .await?;
    let handle = engine
        .start_workflow(
            module,
            Payload::from_json(&json!({ "items": ["only"] }))?,
            std::collections::HashMap::new(),
            "default".to_owned(),
        )
        .await?;
    let workflow_id = handle.workflow_id().clone();
    let run_id = handle.run_id().clone();

    let outcome = tokio::time::timeout(
        Duration::from_secs(30),
        engine.result(&workflow_id, &run_id),
    )
    .await??;
    let parent_error = match outcome {
        Err(error) => error,
        Ok(payload) => {
            return Err(format!(
                "{module} parent must fail, completed with {:?}",
                String::from_utf8_lossy(payload.bytes())
            )
            .into());
        }
    };

    let history = store.read_history(&workflow_id).await?;
    let child_error = history
        .iter()
        .find_map(|event| match event {
            Event::ChildWorkflowFailed { error, .. } => Some(error.clone()),
            _ => None,
        })
        .ok_or_else(|| format!("{module} parent history has no ChildWorkflowFailed"))?;
    engine.shutdown()?;
    Ok(FailureRecord {
        parent_details: details_json(&parent_error)?,
        child_details: details_json(&child_error)?,
    })
}

fn repo_root() -> Result<PathBuf, TestError> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot resolve repository root".into())
}

fn direct_package(source: &str) -> Result<Package, TestError> {
    let prepared = compile_and_assemble_awl(source, Path::new("."))?;
    Ok(Package::load_from_bytes(
        prepared.archive,
        ExtractionLimits::unbounded(),
    )?)
}

/// Build all three scenario documents through the GLEAM path in one scratch
/// project (one `gleam build`, one packaging pass, three archives).
fn gleam_packages() -> Result<Vec<(String, Package)>, TestError> {
    let repo = repo_root()?;
    let root = repo.join("target/direct-failure-parity-gleam");
    fs::create_dir_all(root.join("src"))?;
    fs::create_dir_all(root.join("schemas"))?;

    let scenarios: [(&str, &str, &[&str]); 3] = [
        (STRICT_ABORT_MODULE, STRICT_ABORT, &["stage_one", "stage_two"]),
        (VISITS_MODULE, VISITS, &["deploy", "verify"]),
        (SPAWN_REFUSAL_MODULE, SPAWN_REFUSAL, &["stage_one"]),
    ];

    let mut workflow_toml = String::new();
    for (module, source, activities) in scenarios {
        let document = aion_awl::parse(source)?;
        let diagnostics = aion_awl::check(&document);
        if !diagnostics.is_empty() {
            return Err(format!("{module} did not check: {diagnostics:?}").into());
        }
        let artifact = aion_awl::emit_artifact(&document)?;
        fs::write(root.join("src").join(format!("{module}.gleam")), &artifact.source)?;
        fs::write(
            root.join("src").join(format!("{module}.awl.json")),
            serde_json::to_vec_pretty(&artifact.project_metadata())?,
        )?;
        fs::write(
            root.join("schemas").join(format!("{module}_input.json")),
            serde_json::to_vec_pretty(&aion_awl::schema_for_workflow(&document)?)?,
        )?;
        fs::write(
            root.join("schemas").join(format!("{module}_output.json")),
            serde_json::to_vec_pretty(&aion_awl::schema_for_outcomes(&document)?)?,
        )?;
        let quoted: Vec<String> = activities
            .iter()
            .map(|activity| format!("\"{activity}\""))
            .collect();
        workflow_toml.push_str(&format!(
            "[[workflow]]\nentry_module = \"{module}\"\nentry_function = \"run\"\n\
             timeout_seconds = 60\ninput_schema = \"schemas/{module}_input.json\"\n\
             output_schema = \"schemas/{module}_output.json\"\nactivities = [{}]\n\n",
            quoted.join(", ")
        ));
    }
    fs::write(root.join("workflow.toml"), workflow_toml)?;
    fs::write(
        root.join("gleam.toml"),
        format!(
            "name = \"parity_failure_proof\"\nversion = \"1.0.0\"\ntarget = \"erlang\"\n\n\
             [dependencies]\ngleam_stdlib = \">= 0.44.0 and < 3.0.0\"\ngleam_json = \
             \">= 3.0.0 and < 4.0.0\"\naion_flow = {{ path = \"{}\" }}\n",
            repo.join("gleam/aion_flow").display()
        ),
    )?;
    let output = Command::new("gleam")
        .arg("build")
        .current_dir(&root)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "parity Gleam project did not build:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let report = package_project(&root, &PackageOptions::default())?;
    Ok(report
        .packages
        .into_iter()
        .map(|packaged| (packaged.workflow_type, packaged.package))
        .collect())
}

fn gleam_package_for(packages: &[(String, Package)], module: &str) -> Result<Package, TestError> {
    packages
        .iter()
        .find(|(workflow_type, _)| workflow_type == module)
        .map(|(_, package)| package.clone())
        .ok_or_else(|| format!("gleam packaging produced no archive for {module}").into())
}

/// The three scenarios run sequentially in one test so the shared scratch
/// Gleam project is built exactly once and never raced.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failure_details_converge_between_direct_and_gleam_paths() -> TestResult {
    let gleam = gleam_packages()?;

    // Scenario 1 — strict abort: the typed activity failure crosses the
    // child boundary intact and aborts the parent.
    let direct = run_to_failure(direct_package(STRICT_ABORT)?, STRICT_ABORT_MODULE).await?;
    let reference =
        run_to_failure(gleam_package_for(&gleam, STRICT_ABORT_MODULE)?, STRICT_ABORT_MODULE)
            .await?;
    assert_eq!(direct.parent_details, reference.parent_details);
    assert_eq!(direct.child_details, reference.child_details);
    assert_eq!(
        direct.parent_details,
        json!({ "tag": "AwlActivityFailed", "message": "activity failed" }),
        "strict abort must surface the child's typed activity failure"
    );
    assert_eq!(direct.parent_details, direct.child_details);

    // Scenario 2 — visits exhaustion: the spanned `AwlVisitsExceeded`
    // survives the boundary (the decode-failure regression collapsed it to
    // `AwlChildFailed: child error decode failed: Expected Dict, found
    // List`).
    let direct = run_to_failure(direct_package(VISITS)?, VISITS_MODULE).await?;
    let reference =
        run_to_failure(gleam_package_for(&gleam, VISITS_MODULE)?, VISITS_MODULE).await?;
    assert_eq!(direct.parent_details, reference.parent_details);
    assert_eq!(direct.child_details, reference.child_details);
    assert_eq!(
        direct.parent_details.get("tag").and_then(Value::as_str),
        Some("AwlVisitsExceeded"),
        "the typed kind must survive the parent-child boundary: {:?}",
        direct.parent_details
    );
    let message = direct
        .parent_details
        .get("message")
        .and_then(Value::as_str)
        .ok_or("visits failure carries no message")?;
    assert!(
        message.contains("exceeded its `max … visits` bound"),
        "unexpected visits message: {message}"
    );
    assert_eq!(direct.parent_details, direct.child_details);

    // Scenario 3 — in-child spawn refusal: the engine refuses the child's
    // detached spawn of a type the archive does not carry.
    let direct = run_to_failure(direct_package(SPAWN_REFUSAL)?, SPAWN_REFUSAL_MODULE).await?;
    let reference = run_to_failure(
        gleam_package_for(&gleam, SPAWN_REFUSAL_MODULE)?,
        SPAWN_REFUSAL_MODULE,
    )
    .await?;
    assert_eq!(direct.parent_details, reference.parent_details);
    assert_eq!(direct.child_details, reference.child_details);
    assert_eq!(
        direct.parent_details,
        json!({ "tag": "AwlChildFailed", "message": "detached spawn failed" }),
        "spawn refusal must surface the typed spawn failure"
    );
    assert_eq!(direct.parent_details, direct.child_details);
    Ok(())
}
