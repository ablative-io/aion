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
use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder, WorkflowEntry,
};
use aion_store::{EventStore, InMemoryStore};
use serde_json::{Value, json};

const MODULE: &str = "b5_multi_step_distribute";
const CHILD_TYPE: &str = "awl_distribute_fan_0";
const CHILD_ENTRY: &str = "awl_distribute_fan_0_run";
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

fn emitted_package() -> Result<Package, Box<dyn std::error::Error>> {
    let repo = repo_root()?;
    let root = repo.join("target/flow-vocab-b5-engine-proof");
    fs::create_dir_all(root.join("src"))?;
    let document = aion_awl::parse(AWL)?;
    let diagnostics = aion_awl::check(&document);
    if !diagnostics.is_empty() {
        return Err(format!("engine proof AWL did not check: {diagnostics:?}").into());
    }
    let generated = aion_awl::emit(&document)?;
    if !generated.contains(&format!("workflow.spawn(\"{CHILD_TYPE}\""))
        || !generated.contains(&format!("pub fn {CHILD_ENTRY}"))
    {
        return Err("emitted proof lacks implicit child spawn/entry shape".into());
    }
    fs::write(root.join("src").join(format!("{MODULE}.gleam")), &generated)?;
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
    let mut modules = Vec::new();
    for package in fs::read_dir(root.join("build/dev/erlang"))? {
        let ebin = package?.path().join("ebin");
        if !ebin.is_dir() {
            continue;
        }
        for entry in fs::read_dir(ebin)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("beam") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or("compiled BEAM has no UTF-8 name")?
                .to_owned();
            if name != "aion_flow_ffi" {
                modules.push(BeamModule::new(name, fs::read(&path)?));
            }
        }
    }
    let manifest = Manifest {
        entry_module: MODULE.to_owned(),
        entry_function: "run".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "object" }),
        timeout: Duration::from_secs(30),
        activities: vec![
            DeclaredActivity {
                activity_type: "stage_one".to_owned(),
            },
            DeclaredActivity {
                activity_type: "stage_two".to_owned(),
            },
        ],
        version: ManifestVersion::new("unstamped"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: vec![WorkflowEntry {
            workflow_type: CHILD_TYPE.to_owned(),
            entry_module: MODULE.to_owned(),
            entry_function: CHILD_ENTRY.to_owned(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({}),
            timeout: Duration::from_secs(30),
            internal: true,
        }],
    };
    let archive = PackageBuilder::with_source(
        manifest,
        BeamSet::new(modules)?,
        [(MODULE, generated.into_bytes())],
    )
    .write_to_bytes()?;
    Ok(Package::load_from_bytes(
        archive,
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
    let result_text = std::str::from_utf8(result.bytes())?;
    assert!(
        result_text.contains("\"slots\":3"),
        "tolerant collect must retain three item-order slots: {result_text}"
    );
    let final_history = store.read_history(&workflow_id).await?;
    let starts: Vec<_> = final_history
        .iter()
        .filter(|event| matches!(event, Event::ChildWorkflowStarted { .. }))
        .collect();
    assert_eq!(starts.len(), 3);
    assert!(
        !final_history
            .iter()
            .any(|event| matches!(event, Event::ChildWorkflowCancelled { .. }))
    );
    engine.shutdown()?;
    Ok(())
}
