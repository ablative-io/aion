//! Real-engine proof that emitted multi-step `distribute` regions run as
//! same-package child workflows: all children start before any completes,
//! terminal arrivals may invert item order, tolerant failure preserves a slot,
//! and no sibling is cancelled.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion::signal::ConcreteSignalRouter;
use aion::{EngineBuilder, RuntimeHandle, SignalRouter};
use aion_core::{Event, Payload, WorkflowId};
use aion_package::{Package, PackageOptions, package_project};
use aion_store::{EventStore, InMemoryStore};
use serde_json::{Value, json};

const MODULE: &str = "b5_multi_step_distribute";
const AWL: &str = include_str!("fixtures/b5_multi_step_distribute.awl");
const DEADLINE: Duration = Duration::from_secs(20);
const COLLECTOR_PLACEHOLDER: &str =
    "  Ok(DoneOutcome(Done(first: \"unprojected\", middle_present: True, third: \"unprojected\")))";
const COLLECTOR_PROJECTION: &str = r"  case results {
    [Some(first), None, Some(third)] ->
      Ok(DoneOutcome(Done(first: first, middle_present: False, third: third)))
    _ -> Error(awl_error.AwlFailed)
  }";

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Default)]
struct GateState {
    stage_one: HashSet<String>,
    stage_two: HashSet<String>,
    release_one: bool,
    release_two: HashSet<String>,
    completion_order: Vec<String>,
}

struct Gates {
    state: Mutex<GateState>,
    changed: Condvar,
}

impl Gates {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(GateState::default()),
            changed: Condvar::new(),
        })
    }

    fn wait_until(&self, description: &str, predicate: impl Fn(&GateState) -> bool) -> TestResult {
        let deadline = std::time::Instant::now() + DEADLINE;
        let mut state = self.state.lock().map_err(|_| "gate state lock poisoned")?;
        while !predicate(&state) {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .ok_or_else(|| format!("timed out waiting for {description}"))?;
            let (next, _) = self
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| "gate state lock poisoned")?;
            state = next;
        }
        Ok(())
    }

    fn release_stage_one(&self) -> TestResult {
        let mut state = self.state.lock().map_err(|_| "gate state lock poisoned")?;
        state.release_one = true;
        self.changed.notify_all();
        Ok(())
    }

    fn release_stage_two(&self, item: &str) -> TestResult {
        let mut state = self.state.lock().map_err(|_| "gate state lock poisoned")?;
        state.release_two.insert(item.to_owned());
        self.changed.notify_all();
        Ok(())
    }

    fn completion_order(&self) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| "gate state lock poisoned")?
            .completion_order
            .clone())
    }
}

struct GatedDispatcher {
    gates: Arc<Gates>,
}

impl ActivityDispatcher for GatedDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        let value: Value =
            serde_json::from_str(&request.input).map_err(|error| error.to_string())?;
        let item = value
            .get("item")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("activity {} input has no string item", request.name))?;
        match request.name.as_str() {
            "stage_one" => {
                let mut state = self
                    .gates
                    .state
                    .lock()
                    .map_err(|_| "gate state lock poisoned".to_owned())?;
                state.stage_one.insert(item.to_owned());
                self.gates.changed.notify_all();
                while !state.release_one {
                    state = self
                        .gates
                        .changed
                        .wait(state)
                        .map_err(|_| "gate state lock poisoned".to_owned())?;
                }
                serde_json::to_string(&format!("{item}-one")).map_err(|error| error.to_string())
            }
            "stage_two" => {
                let key = item.strip_suffix("-one").unwrap_or(item).to_owned();
                let mut state = self
                    .gates
                    .state
                    .lock()
                    .map_err(|_| "gate state lock poisoned".to_owned())?;
                state.stage_two.insert(key.clone());
                self.gates.changed.notify_all();
                while !state.release_two.contains(&key) {
                    state = self
                        .gates
                        .changed
                        .wait(state)
                        .map_err(|_| "gate state lock poisoned".to_owned())?;
                }
                state.completion_order.push(key.clone());
                self.gates.changed.notify_all();
                if key == "b" {
                    Err("intentional-b".to_owned())
                } else {
                    serde_json::to_string(&format!("{key}-done")).map_err(|error| error.to_string())
                }
            }
            other => Err(format!("unknown proof activity {other}")),
        }
    }
}

fn repo_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot resolve repository root".into())
}

/// The language intentionally forbids `[T?]` in public types, but tolerant
/// collection owns that shape internally. Replace the fixture's legal
/// placeholder tail with a test-only pattern match in the emitted workflow so
/// the engine result is produced from the collector's actual three slots.
fn expose_actual_collector_slots(source: &str) -> Result<String, Box<dyn std::error::Error>> {
    if source.matches(COLLECTOR_PLACEHOLDER).count() != 1 {
        return Err("emitted proof lacks its unique collector placeholder tail".into());
    }
    Ok(source.replacen(COLLECTOR_PLACEHOLDER, COLLECTOR_PROJECTION, 1))
}

fn emitted_package() -> Result<Package, Box<dyn std::error::Error>> {
    let repo = repo_root()?;
    let root = repo.join("target/flow-vocab-b5-engine-proof");
    fs::create_dir_all(root.join("src"))?;
    fs::create_dir_all(root.join("schemas"))?;
    let document = aion_awl::parse(AWL)?;
    let diagnostics = aion_awl::check(&document);
    if !diagnostics.is_empty() {
        return Err(format!("engine proof AWL did not check: {diagnostics:?}").into());
    }
    let artifact = aion_awl::emit_artifact(&document)?;
    let projected_source = expose_actual_collector_slots(&artifact.source)?;
    let [child] = artifact.synthesized_workflows.as_slice() else {
        return Err(format!(
            "expected exactly one synthesized entry, got {}",
            artifact.synthesized_workflows.len()
        )
        .into());
    };
    if !artifact
        .source
        .contains(&format!("workflow.spawn(\"{}\"", child.workflow_type))
        || !artifact
            .source
            .contains(&format!("pub fn {}", child.entry_function))
    {
        return Err("emitted proof lacks its structured child spawn/entry shape".into());
    }
    fs::write(
        root.join("src").join(format!("{MODULE}.gleam")),
        projected_source,
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
            "[[workflow]]\nentry_module = \"{MODULE}\"\nentry_function = \"run\"\ntimeout_seconds = 30\ninput_schema = \"schemas/input.json\"\noutput_schema = \"schemas/output.json\"\nactivities = [\"stage_one\", \"stage_two\"]\n"
        ),
    )?;
    fs::write(
        root.join("gleam.toml"),
        format!(
            "name = \"{MODULE}\"\nversion = \"1.0.0\"\ntarget = \"erlang\"\n\n[dependencies]\ngleam_stdlib = \">= 0.44.0 and < 3.0.0\"\ngleam_json = \">= 3.0.0 and < 4.0.0\"\naion_flow = {{ path = \"{}\" }}\n",
            repo.join("gleam/aion_flow").display()
        ),
    )?;
    let output = Command::new("gleam")
        .arg("build")
        .current_dir(&root)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "emitted engine proof did not build:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let mut report = package_project(&root, &PackageOptions::default())?;
    let packaged = report
        .packages
        .pop()
        .ok_or("project packaging produced no archive")?;
    if !report.packages.is_empty() {
        return Err("project packaging produced more than one archive".into());
    }
    let entries = &packaged.package.manifest().additional_workflows;
    if entries.len() != 1
        || entries[0].workflow_type != child.workflow_type
        || entries[0].entry_function != child.entry_function
        || entries[0].input_schema != child.input_schema
        || entries[0].output_schema != child.output_schema
    {
        return Err("packaged synthesized entry differs from emitted artifact".into());
    }
    Ok(packaged.package)
}

async fn wait_history(
    store: &Arc<dyn EventStore>,
    workflow_id: &WorkflowId,
    predicate: impl Fn(&[Event]) -> bool,
) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let history = store.read_history(workflow_id).await?;
        if predicate(&history) {
            return Ok(history);
        }
        if std::time::Instant::now() > deadline {
            return Err(format!("timed out waiting for history: {history:#?}").into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn assert_durable_child_histories(
    store: &Arc<dyn EventStore>,
    parent_history: &[Event],
) -> Result<Vec<Option<String>>, Box<dyn std::error::Error>> {
    let mut children = Vec::new();
    for event in parent_history {
        if let Event::ChildWorkflowStarted {
            child_workflow_id,
            input,
            ..
        } = event
        {
            let value: Value = serde_json::from_slice(input.bytes())?;
            let item = value
                .get("item")
                .and_then(Value::as_str)
                .ok_or("child input has no item")?;
            children.push((item.to_owned(), child_workflow_id.clone()));
        }
    }
    assert_eq!(children.len(), 3);
    assert!(
        !parent_history
            .iter()
            .any(|event| matches!(event, Event::ChildWorkflowCancelled { .. })),
        "parent history contains a child cancellation"
    );

    // Reconstruct the tolerant collect through the durable parent seam, in
    // source-item order. This exposes the internal `[String?]` without
    // weakening the language rule that forbids optional list elements in
    // public outcomes.
    let mut durable_slots = Vec::with_capacity(children.len());
    for (item, child_id) in &children {
        let terminal = parent_history.iter().find(|event| match event {
            Event::ChildWorkflowCompleted {
                child_workflow_id, ..
            }
            | Event::ChildWorkflowFailed {
                child_workflow_id, ..
            } => child_workflow_id == child_id,
            _ => false,
        });
        match terminal {
            Some(Event::ChildWorkflowCompleted { result, .. }) => {
                let value: Value = serde_json::from_slice(result.bytes())?;
                let output = value
                    .get("payload")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("child {item} completion has no string payload"))?;
                durable_slots.push(Some(output.to_owned()));
            }
            Some(Event::ChildWorkflowFailed { error, .. }) => {
                assert_eq!(item, "b", "only item b should fail: {error:?}");
                let details = error
                    .details
                    .as_ref()
                    .ok_or_else(|| format!("item b failure has no details: {error:?}"))?;
                // The run shell records failures as the `AwlError` codec's
                // own JSON-object encoding (the decodable child-error wire),
                // never the raw error record's term-to-JSON array image.
                assert_eq!(
                    serde_json::from_slice::<Value>(details.bytes())?,
                    json!({ "tag": "AwlActivityFailed", "message": "activity failed" }),
                    "item b must hold the generated child workflow's durable activity failure"
                );
                durable_slots.push(None);
            }
            _ => return Err(format!("child {item} has no durable terminal event").into()),
        }
    }

    assert_durable_child_overlap(store, &children).await?;
    Ok(durable_slots)
}

async fn assert_durable_child_overlap(
    store: &Arc<dyn EventStore>,
    children: &[(String, WorkflowId)],
) -> TestResult {
    let mut first_schedules = Vec::new();
    let mut first_completions = Vec::new();
    for (item, child_id) in children {
        let history = store.read_history(child_id).await?;
        assert!(
            !history.iter().any(|event| matches!(
                event,
                Event::ChildWorkflowCancelled { .. } | Event::WorkflowCancelled { .. }
            )),
            "child {item} history contains cancellation"
        );
        let (activity_id, scheduled_at, scheduled_item) = history
            .iter()
            .find_map(|event| match event {
                Event::ActivityScheduled {
                    activity_id,
                    activity_type,
                    input,
                    ..
                } if activity_type == "stage_one" => {
                    let value: Value = serde_json::from_slice(input.bytes()).ok()?;
                    Some((
                        activity_id.clone(),
                        *event.recorded_at(),
                        value.get("item")?.as_str()?.to_owned(),
                    ))
                }
                _ => None,
            })
            .ok_or_else(|| format!("child {item} has no durable stage_one schedule"))?;
        assert_eq!(scheduled_item, *item);
        let completed_at = history
            .iter()
            .find_map(|event| match event {
                Event::ActivityCompleted {
                    activity_id: completed,
                    ..
                } if *completed == activity_id => Some(*event.recorded_at()),
                _ => None,
            })
            .ok_or_else(|| format!("child {item} has no durable stage_one completion"))?;
        first_schedules.push(scheduled_at);
        first_completions.push(completed_at);
    }
    assert!(
        first_schedules.iter().max() < first_completions.iter().min(),
        "all three durable first schedules must precede the first completion: schedules={first_schedules:?}, completions={first_completions:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn emitted_multistep_distribute_is_parallel_ordered_and_tolerant() -> TestResult {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let gates = Gates::new();
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(4)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .activity_dispatcher(Arc::new(GatedDispatcher {
            gates: Arc::clone(&gates),
        }))
        .load_workflows(emitted_package()?)
        .build()
        .await?;
    let handle = engine
        .start_workflow(
            MODULE,
            Payload::from_json(&json!({ "items": ["a", "b", "c"] }))?,
            std::collections::HashMap::new(),
            "default".to_owned(),
        )
        .await?;
    let workflow_id = handle.workflow_id().clone();
    let run_id = handle.run_id().clone();

    gates.wait_until("all first activities dispatched", |state| {
        state.stage_one.len() == 3
    })?;
    let spawned = wait_history(&store, &workflow_id, |events| {
        events
            .iter()
            .filter(|event| matches!(event, Event::ChildWorkflowStarted { .. }))
            .count()
            == 3
    })
    .await?;
    let first_terminal = spawned.iter().position(|event| {
        matches!(
            event,
            Event::ChildWorkflowCompleted { .. } | Event::ChildWorkflowFailed { .. }
        )
    });
    assert!(
        first_terminal.is_none(),
        "a child completed before all three dispatched"
    );

    gates.release_stage_one()?;
    gates.wait_until("all second activities dispatched", |state| {
        state.stage_two.len() == 3
    })?;
    gates.release_stage_two("c")?;
    gates.wait_until("c completion", |state| state.completion_order.len() == 1)?;
    gates.release_stage_two("b")?;
    gates.wait_until("b failure", |state| state.completion_order.len() == 2)?;
    let partial = store.read_history(&workflow_id).await?;
    assert_eq!(
        gates.completion_order()?,
        vec!["c", "b"],
        "c completed and b failed while item-order sibling a remained blocked"
    );
    assert!(
        !partial
            .iter()
            .any(|event| matches!(event, Event::ChildWorkflowCancelled { .. })),
        "tolerant failure must not cancel siblings"
    );
    gates.release_stage_two("a")?;

    let result = engine
        .result(&workflow_id, &run_id)
        .await?
        .map_err(|error| format!("parent failed: {error:?}"))?;
    assert_eq!(gates.completion_order()?, vec!["c", "b", "a"]);
    let decoded_result: Value = serde_json::from_slice(result.bytes())?;
    let projected = decoded_result
        .get("payload")
        .ok_or("parent result has no projected collector payload")?;
    assert_eq!(
        projected.get("first").and_then(Value::as_str),
        Some("a-done")
    );
    assert_eq!(
        projected.get("middle_present").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        projected.get("third").and_then(Value::as_str),
        Some("c-done")
    );

    // Correlate the result produced by the actual collector with the separate
    // parent/child durable-event reconstruction.
    let final_history = store.read_history(&workflow_id).await?;
    let durable_slots = assert_durable_child_histories(&store, &final_history).await?;
    assert_eq!(
        durable_slots,
        vec![Some("a-done".to_owned()), None, Some("c-done".to_owned())],
        "tolerant collect must preserve exact ordered optional slots"
    );
    engine.shutdown()?;
    Ok(())
}
