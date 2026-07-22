use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aion_core::{
    ActivityId, ContentType, Event, EventEnvelope, Payload, RunId, WorkflowId, WorkflowStatus,
};
use aion_package::ContentHash;
use aion_store::{EventStore, InMemoryStore, WriteToken};
use beamr::loader::Instruction;
use beamr::loader::decode::compact::Operand;
use beamr::module::{Module, ResolvedImport, ResolvedImportTarget};
use beamr::native::ProcessContext;
use beamr::term::Term;

use super::nif_activity_await::{ActivityAwaitStep, await_activity_step};
use super::nif_activity_dispatch::dispatch_activity_impl;
use super::nif_context::NifContext;
use super::outcome::WorkflowProcessOutcome;
use super::{Mfa, NifEntry, NifRegistration, RuntimeConfig, RuntimeHandle, RuntimeInput};
use crate::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use crate::durability::Recorder;
use crate::registry::{
    CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const TEST_POLL: Duration = Duration::from_millis(1);
static FAST_GATE_CALLS: AtomicUsize = AtomicUsize::new(0);
static FAST_GATE_ENTERED: AtomicBool = AtomicBool::new(false);
static FAST_GATE_RELEASED: AtomicBool = AtomicBool::new(false);
static POISON_GATE_CALLS: AtomicUsize = AtomicUsize::new(0);
static POISON_GATE_ENTERED: AtomicBool = AtomicBool::new(false);
static POISON_GATE_REENTERED: AtomicBool = AtomicBool::new(false);
static POISON_GATE_RELEASED: AtomicBool = AtomicBool::new(false);

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct ImmediateDispatcher;

impl ActivityDispatcher for ImmediateDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        if request.name == "fast" {
            Ok(r#""fast""#.to_owned())
        } else {
            Err(format!("unexpected activity {}", request.name))
        }
    }
}

fn fast_gate(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let Some(first) = args.first().copied() else {
        return Err(Term::NIL);
    };
    if FAST_GATE_CALLS.fetch_add(1, Ordering::AcqRel) == 0 {
        FAST_GATE_ENTERED.store(true, Ordering::Release);
        context.request_suspend(None);
        return Ok(Term::NIL);
    }
    if !FAST_GATE_RELEASED.load(Ordering::Acquire) {
        context.request_suspend(None);
        return Ok(Term::NIL);
    }
    Ok(first)
}

fn poison_gate(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let Some(first) = args.first().copied() else {
        return Err(Term::NIL);
    };
    if POISON_GATE_CALLS.fetch_add(1, Ordering::AcqRel) == 0 {
        POISON_GATE_ENTERED.store(true, Ordering::Release);
        context.request_suspend(None);
        return Ok(Term::NIL);
    }
    POISON_GATE_REENTERED.store(true, Ordering::Release);
    let deadline = Instant::now() + TEST_TIMEOUT;
    while !POISON_GATE_RELEASED.load(Ordering::Acquire) && Instant::now() < deadline {
        std::thread::sleep(TEST_POLL);
    }
    if POISON_GATE_RELEASED.load(Ordering::Acquire) {
        Ok(first)
    } else {
        Err(Term::NIL)
    }
}

fn held_dispatch(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let pid = context.pid().ok_or(Term::NIL)?;
    let state = super::nif_state::engine_nif_state(context).map_err(|_| Term::NIL)?;
    let runtime = super::nif_activity::runtime_context(&state)
        .map_err(|_| Term::NIL)?
        .runtime;
    let result = dispatch_activity_impl(args, context);
    let deadline = Instant::now() + TEST_TIMEOUT;
    while runtime.activity_result(pid, 0).is_none() && Instant::now() < deadline {
        std::thread::sleep(TEST_POLL);
    }
    if runtime.activity_result(pid, 0).is_none() {
        Err(Term::NIL)
    } else {
        result
    }
}

#[test]
fn fast_completion_during_dispatch_nif_resolves_on_next_real_await() -> TestResult {
    reset_fast_gate();
    let tokio_runtime = tokio::runtime::Runtime::new()?;
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(2)))?);
    install_test_nifs(&runtime)?;
    register_dispatch_then_await_module(&runtime)?;
    let registry = Arc::new(Registry::default());
    super::install_nif_runtime_context(
        runtime.nif_state(),
        Arc::clone(&registry),
        Arc::clone(&runtime),
        tokio_runtime.handle().clone(),
    );
    runtime
        .nif_state()
        .set_activity_dispatcher(Arc::new(ImmediateDispatcher));
    let input = RuntimeInput::from_payloads_for_test(&[
        json_payload(b"fast"),
        json_payload(br#"{"input":true}"#),
        json_payload(br#"{"retry":null}"#),
    ])?;
    let pid = runtime.spawn_workflow("round6_dispatch", "run", input)?;
    wait_until(
        || FAST_GATE_ENTERED.load(Ordering::Acquire),
        "fast gate did not suspend",
    )?;
    register_workflow(&tokio_runtime, &registry, pid, false)?;
    runtime.force_activity_marker_refusals_for_test(
        pid,
        0,
        runtime.signal_delivery().max_enqueue_attempts,
    );
    let (sender, receiver) = std::sync::mpsc::channel();
    runtime.monitor_process_for_test(pid, move |outcome| {
        if sender.send(outcome).is_err() {
            tracing::error!("fast completion monitor receiver dropped");
        }
    })?;
    FAST_GATE_RELEASED.store(true, Ordering::Release);
    runtime.deliver_signal_received(pid)?;

    let outcome = receiver.recv_timeout(TEST_TIMEOUT)??;
    let WorkflowProcessOutcome::Completed(payload) = outcome else {
        return Err("fast completion workflow failed instead of resolving await".into());
    };
    assert_eq!(payload.bytes(), br#""fast""#);
    assert_eq!(runtime.retained_activity_completions(), 0);
    assert_eq!(runtime.retained_activity_attempt_count_for_test(), 0);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn in_vm_spawn_failure_fallback_resolves_failed_never_suspend() -> TestResult {
    let tokio_runtime = tokio::runtime::Runtime::new()?;
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(1)))?);
    let registry = Arc::new(Registry::default());
    let pid = runtime.spawn_test_process()?;
    register_workflow(&tokio_runtime, &registry, pid, true)?;
    runtime.force_activity_marker_refusals_for_test(
        pid,
        0,
        runtime.signal_delivery().max_enqueue_attempts,
    );
    super::nif_activity_in_vm::retain_spawn_failure(
        &runtime,
        pid,
        "activity:0",
        &"forced closure spawn rejection",
    );
    let mut context = NifContext::new(
        pid,
        &registry,
        tokio_runtime.handle().clone(),
        runtime.signal_delivery(),
    )?;

    let step = tokio_runtime.block_on(async {
        tokio::task::block_in_place(|| {
            await_activity_step(
                runtime.nif_state(),
                &mut context,
                &runtime,
                &ActivityId::from_sequence_position(0),
                || {},
            )
        })
    })?;
    assert!(matches!(
        step,
        ActivityAwaitStep::Failed(message)
            if message.contains("terminal:in-vm activity child spawn failed")
    ));
    assert_eq!(runtime.retained_activity_completions(), 0);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn real_await_nif_poison_fails_process_and_monitor_drains_state() -> TestResult {
    reset_poison_gate();
    let tokio_runtime = tokio::runtime::Runtime::new()?;
    let runtime = Arc::new(RuntimeHandle::new(RuntimeConfig::new(Some(2)))?);
    install_test_nifs(&runtime)?;
    register_poison_await_module(&runtime)?;
    let registry = Arc::new(Registry::default());
    super::install_nif_runtime_context(
        runtime.nif_state(),
        Arc::clone(&registry),
        Arc::clone(&runtime),
        tokio_runtime.handle().clone(),
    );
    let input = RuntimeInput::from_payload(&json_payload(b"activity:0"))?;
    let pid = runtime.spawn_workflow("round6_poison", "run", input)?;
    wait_until(
        || POISON_GATE_ENTERED.load(Ordering::Acquire),
        "poison gate did not suspend",
    )?;
    register_workflow(&tokio_runtime, &registry, pid, true)?;
    let baseline = runtime.activity_delivery_gate_count();
    let (sender, receiver) = std::sync::mpsc::channel();
    runtime.monitor_process_for_test(pid, move |outcome| {
        if sender.send(outcome).is_err() {
            tracing::error!("poison monitor receiver dropped");
        }
    })?;
    runtime.deliver_activity_completion_message_with_attempt(
        pid,
        "activity:0",
        r#""retained""#.to_owned(),
        Some(4),
    )?;
    runtime.force_activity_delivery_poison_for_test(pid)?;
    wait_until(
        || POISON_GATE_REENTERED.load(Ordering::Acquire),
        "poison gate was not re-entered by the queued marker",
    )?;
    POISON_GATE_RELEASED.store(true, Ordering::Release);

    let monitored = receiver.recv_timeout(TEST_TIMEOUT)?;
    assert!(matches!(
        monitored,
        Err(crate::EngineError::ActivityDeliveryPoisoned { process_id }) if process_id == pid
    ));
    assert!(!runtime.is_live(pid));
    assert_eq!(runtime.retained_activity_completions(), 0);
    assert_eq!(runtime.retained_activity_attempt_count_for_test(), 0);
    assert_eq!(runtime.activity_delivery_gate_count(), baseline);
    runtime.shutdown()?;
    Ok(())
}

fn install_test_nifs(runtime: &RuntimeHandle) -> TestResult {
    let mut registration = NifRegistration::new();
    registration.add_engine_nifs();
    registration.add_host_nifs([
        NifEntry::new(Mfa::new("round6_test", "fast_gate", 3), fast_gate),
        NifEntry::new(Mfa::new("round6_test", "held_dispatch", 3), held_dispatch),
        NifEntry::new(Mfa::new("round6_test", "poison_gate", 1), poison_gate),
    ]);
    runtime.install_nifs(registration)?;
    Ok(())
}

fn register_dispatch_then_await_module(runtime: &RuntimeHandle) -> TestResult {
    register_test_module(
        runtime,
        "round6_dispatch",
        3,
        &[
            ("round6_test", "fast_gate", 3),
            ("round6_test", "held_dispatch", 3),
            ("aion_flow_ffi", "await_activity_result", 1),
        ],
        vec![
            call_ext(3, 0),
            call_ext(3, 1),
            Instruction::GetTupleElement {
                source: Operand::X(0),
                index: Operand::Unsigned(1),
                destination: Operand::X(0),
            },
            call_ext(1, 2),
        ],
    )
}

fn register_poison_await_module(runtime: &RuntimeHandle) -> TestResult {
    register_test_module(
        runtime,
        "round6_poison",
        1,
        &[
            ("round6_test", "poison_gate", 1),
            ("aion_flow_ffi", "await_activity_result", 1),
        ],
        vec![call_ext(1, 0), call_ext(1, 1)],
    )
}

fn register_test_module(
    runtime: &RuntimeHandle,
    module_name: &str,
    arity: u8,
    imports: &[(&str, &str, u8)],
    mut body: Vec<Instruction>,
) -> TestResult {
    let module = runtime.atom_table.intern(module_name);
    let function = runtime.atom_table.intern("run");
    let mut code = vec![Instruction::Label { label: 1 }];
    code.append(&mut body);
    code.push(Instruction::Return);
    let mut resolved_imports = Vec::new();
    for (native_module, native_function, native_arity) in imports {
        let entry = runtime
            .lookup_native_for_test(native_module, native_function, *native_arity)
            .ok_or_else(|| {
                format!("missing native {native_module}:{native_function}/{native_arity}")
            })?;
        resolved_imports.push(ResolvedImport {
            module: runtime.atom_table.intern(native_module),
            function: runtime.atom_table.intern(native_function),
            arity: *native_arity,
            target: ResolvedImportTarget::Native(entry),
        });
    }
    runtime.module_registry.insert(Module {
        name: module,
        generation: 0,
        origin: beamr::module::ModuleOrigin::Preloaded,
        exports: std::collections::HashMap::from([((function, arity), 1)]),
        label_index: std::collections::HashMap::from([(1, 0)]),
        code,
        function_table: Vec::new(),
        line_table: Vec::new(),
        literals: Vec::new(),
        constant_pool: beamr::constant_pool::ConstantPool::new(),
        resolved_imports,
        lambdas: Vec::new(),
        string_table: Vec::new(),
        line_info: Vec::new(),
    });
    Ok(())
}

fn register_workflow(
    tokio_runtime: &tokio::runtime::Runtime,
    registry: &Registry,
    pid: u64,
    pending_activity: bool,
) -> TestResult {
    let workflow_id = WorkflowId::new_v4();
    let run_id = RunId::new_v4();
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let mut events = vec![Event::WorkflowStarted {
        envelope: envelope(&workflow_id, 1),
        workflow_type: "round6".to_owned(),
        input: json_payload(b"null"),
        run_id: run_id.clone(),
        parent_run_id: None,
        package_version: aion_core::PackageVersion::new("a".repeat(64)),
    }];
    if pending_activity {
        events.push(Event::ActivityScheduled {
            envelope: envelope(&workflow_id, 2),
            activity_id: ActivityId::from_sequence_position(0),
            activity_type: "work".to_owned(),
            input: json_payload(b"null"),
            task_queue: "default".to_owned(),
            node: None,
        });
        events.push(Event::ActivityStarted {
            envelope: envelope(&workflow_id, 3),
            activity_id: ActivityId::from_sequence_position(0),
            attempt: 1,
        });
    }
    tokio_runtime.block_on(store.append(WriteToken::recorder(), &workflow_id, &events, 0))?;
    let recorder = Recorder::resume_at(workflow_id.clone(), store, u64::try_from(events.len())?);
    registry.insert(
        (workflow_id.clone(), run_id.clone()),
        WorkflowHandle::new(WorkflowHandleParts {
            workflow_id,
            run_id,
            pid,
            workflow_type: "round6".to_owned(),
            namespace: "default".to_owned(),
            loaded_version: ContentHash::from_bytes([7; 32]),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        }),
    )?;
    Ok(())
}

fn envelope(workflow_id: &WorkflowId, seq: u64) -> EventEnvelope {
    EventEnvelope {
        seq,
        recorded_at: chrono::Utc::now(),
        workflow_id: workflow_id.clone(),
    }
}

fn call_ext(arity: u8, import: u64) -> Instruction {
    Instruction::CallExt {
        arity: Operand::Unsigned(arity.into()),
        import: Operand::Unsigned(import),
    }
}

fn json_payload(bytes: &[u8]) -> Payload {
    Payload::new(ContentType::Json, bytes.to_vec())
}

fn wait_until(mut condition: impl FnMut() -> bool, message: &'static str) -> TestResult {
    let deadline = Instant::now() + TEST_TIMEOUT;
    while Instant::now() < deadline {
        if condition() {
            return Ok(());
        }
        std::thread::sleep(TEST_POLL);
    }
    Err(message.into())
}

fn reset_fast_gate() {
    FAST_GATE_CALLS.store(0, Ordering::Release);
    FAST_GATE_ENTERED.store(false, Ordering::Release);
    FAST_GATE_RELEASED.store(false, Ordering::Release);
}

fn reset_poison_gate() {
    POISON_GATE_CALLS.store(0, Ordering::Release);
    POISON_GATE_ENTERED.store(false, Ordering::Release);
    POISON_GATE_REENTERED.store(false, Ordering::Release);
    POISON_GATE_RELEASED.store(false, Ordering::Release);
}
