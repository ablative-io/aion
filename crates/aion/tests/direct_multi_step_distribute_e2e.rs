//! Real-engine proof that DIRECT-compiled multi-step `distribute` regions run
//! as same-package child workflows — the bytecode-path mirror of
//! `multi_step_distribute_e2e`: the archive comes from
//! `compile_and_assemble_awl` (source → MIR → BEAM, no Gleam toolchain), all
//! children start before any completes, terminal arrivals may invert item
//! order, tolerant failure preserves a positional slot, and no sibling is
//! cancelled. A second test pins archive-level parity: the direct archive and
//! the Gleam-path archive for the same document expose identical workflow
//! types and entries.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion::signal::ConcreteSignalRouter;
use aion::{EngineBuilder, RuntimeHandle, SignalRouter};
use aion_awl_package::compile_and_assemble_awl;
use aion_core::{Event, Payload, WorkflowId};
use aion_package::{ExtractionLimits, Package, PackageOptions, package_project};
use aion_store::{EventStore, InMemoryStore};
use serde_json::{Value, json};

const MODULE: &str = "b5_multi_step_distribute";
const AWL: &str = include_str!("fixtures/b5_multi_step_distribute.awl");
const DEADLINE: Duration = Duration::from_secs(20);

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

/// The DIRECT-path archive: source → MIR → BEAM → `.aion`, no toolchain.
fn direct_package() -> Result<Package, Box<dyn std::error::Error>> {
    let prepared = compile_and_assemble_awl(AWL, Path::new("."))?;
    let [child] = prepared.compiled.synthesized_workflows.as_slice() else {
        return Err(format!(
            "expected exactly one synthesized entry, got {}",
            prepared.compiled.synthesized_workflows.len()
        )
        .into());
    };
    if child.entry_module != MODULE {
        return Err("synthesized entry names the wrong module".into());
    }
    Ok(Package::load_from_bytes(
        prepared.archive,
        ExtractionLimits::unbounded(),
    )?)
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

/// Reconstruct the tolerant collect through the durable parent seam, in
/// source-item order, and assert no cancellation reached any child.
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

/// The direct-path engine proof: 3 items fan out as real child workflows
/// (spawn-all before any await), tolerant failure of the middle item leaves
/// `[Some, None, Some]` durable slots in item order, no sibling is cancelled,
/// and the parent completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn direct_multistep_distribute_is_parallel_ordered_and_tolerant() -> TestResult {
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
        .load_workflows(direct_package()?)
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
    // The fixture's collector tail routes literal placeholder values — the
    // real per-slot data is asserted below through the durable seam.
    let projected = decoded_result
        .get("payload")
        .ok_or("parent result has no collector payload")?;
    assert_eq!(
        projected.get("first").and_then(Value::as_str),
        Some("unprojected")
    );
    assert_eq!(
        projected.get("middle_present").and_then(Value::as_bool),
        Some(true)
    );

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

/// Archive-level parity: the DIRECT archive and the GLEAM-path archive for
/// the same document expose identical workflow types and entries (routing
/// identity, entry function, schemas, internal marking).
#[test]
fn direct_and_gleam_archives_expose_identical_workflow_entries() -> TestResult {
    let prepared = compile_and_assemble_awl(AWL, Path::new("."))?;
    let direct = Package::load_from_bytes(prepared.archive, ExtractionLimits::unbounded())?;
    let gleam = gleam_package()?;

    let direct_manifest = direct.manifest();
    let gleam_manifest = gleam.manifest();
    assert_eq!(direct_manifest.entry_module, gleam_manifest.entry_module);
    assert_eq!(
        direct_manifest.additional_workflows.len(),
        gleam_manifest.additional_workflows.len(),
        "entry counts differ between paths"
    );
    for (direct_entry, gleam_entry) in direct_manifest
        .additional_workflows
        .iter()
        .zip(&gleam_manifest.additional_workflows)
    {
        assert_eq!(direct_entry.workflow_type, gleam_entry.workflow_type);
        assert_eq!(direct_entry.entry_module, gleam_entry.entry_module);
        assert_eq!(direct_entry.entry_function, gleam_entry.entry_function);
        assert_eq!(direct_entry.input_schema, gleam_entry.input_schema);
        assert_eq!(direct_entry.output_schema, gleam_entry.output_schema);
        assert_eq!(direct_entry.internal, gleam_entry.internal);
    }
    Ok(())
}

/// Build the GLEAM-path archive for the same document (its own scratch
/// project dir so the sibling e2e's build tree is never raced).
fn gleam_package() -> Result<Package, Box<dyn std::error::Error>> {
    let repo = repo_root()?;
    let root = repo.join("target/direct-parity-gleam-proof");
    fs::create_dir_all(root.join("src"))?;
    fs::create_dir_all(root.join("schemas"))?;
    let document = aion_awl::parse(AWL)?;
    let diagnostics = aion_awl::check(&document);
    if !diagnostics.is_empty() {
        return Err(format!("parity AWL did not check: {diagnostics:?}").into());
    }
    let artifact = aion_awl::emit_artifact(&document)?;
    fs::write(
        root.join("src").join(format!("{MODULE}.gleam")),
        &artifact.source,
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
            "parity Gleam project did not build:\n{}\n{}",
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
    Ok(packaged.package)
}
