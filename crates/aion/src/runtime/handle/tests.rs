use aion_core::Payload;
use std::time::Duration;

use beamr::loader::Instruction;
use beamr::loader::decode::compact::Operand;
use beamr::module::{Module, ResolvedImport, ResolvedImportTarget};
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary_ref::BinaryRef;

use super::{RuntimeHandle, RuntimeInput};
use crate::error::EngineError;
use crate::runtime::{Mfa, NifEntry, NifRegistration, RuntimeConfig, SignalDeliveryConfig};

fn forty_two(args: &[Term], _: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::small_int(0));
    }
    Ok(Term::small_int(42))
}

fn thirteen(args: &[Term], _: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::small_int(0));
    }
    Ok(Term::small_int(13))
}

fn binary_length(args: &[Term], _: &mut ProcessContext) -> Result<Term, Term> {
    match args {
        [term] => BinaryRef::new(*term)
            .and_then(|binary| i64::try_from(binary.as_bytes().len()).ok())
            .map(Term::small_int)
            .ok_or_else(|| Term::small_int(0)),
        _ => Err(Term::small_int(0)),
    }
}

fn native_call_module_for_test(
    module: beamr::atom::Atom,
    function: beamr::atom::Atom,
    target_module: beamr::atom::Atom,
    target_function: beamr::atom::Atom,
    native_entry: Option<beamr::native::NativeEntry>,
) -> Module {
    native_call_module_with_arity_for_test(
        module,
        function,
        target_module,
        target_function,
        0,
        native_entry,
    )
}

fn native_call_module_with_arity_for_test(
    module: beamr::atom::Atom,
    function: beamr::atom::Atom,
    target_module: beamr::atom::Atom,
    target_function: beamr::atom::Atom,
    arity: u8,
    native_entry: Option<beamr::native::NativeEntry>,
) -> Module {
    let label = 1;
    let code = vec![
        Instruction::Label { label },
        Instruction::CallExt {
            arity: Operand::Unsigned(arity.into()),
            import: Operand::Unsigned(0),
        },
        Instruction::Return,
    ];
    let mut module_data = Module {
        name: module,
        generation: 0,
        origin: beamr::module::ModuleOrigin::Preloaded,
        exports: std::collections::HashMap::from([((function, arity), label)]),
        label_index: std::collections::HashMap::from([(label, 0)]),
        code,
        function_table: Vec::new(),
        line_table: Vec::new(),
        literals: Vec::new(),
        constant_pool: beamr::constant_pool::ConstantPool::new(),
        resolved_imports: Vec::new(),
        lambdas: Vec::new(),
        string_table: Vec::new(),
        line_info: Vec::new(),
    };
    if let Some(native_entry) = native_entry {
        module_data.resolved_imports.push(ResolvedImport {
            module: target_module,
            function: target_function,
            arity,
            target: ResolvedImportTarget::Native(native_entry),
        });
    }
    module_data
}

fn assert_send_sync<T: Send + Sync>() {}

fn fixture_workflow_beam() -> &'static [u8] {
    include_bytes!("../../../tests/fixtures/aion_fixture_workflow.beam")
}

#[test]
fn runtime_handle_is_send_sync() {
    assert_send_sync::<RuntimeHandle>();
}

#[test]
fn registers_spawns_and_shuts_down() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    runtime.register_module("aion_fixture_workflow", fixture_workflow_beam())?;

    let pid = runtime.spawn_workflow("aion_fixture_workflow", "wait", RuntimeInput::default())?;
    assert!(runtime.cancel_pid(pid).is_ok());
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn signal_delivery_to_dead_process_returns_typed_error() -> Result<(), Box<dyn std::error::Error>> {
    let signal_delivery =
        SignalDeliveryConfig::new(Duration::ZERO, 1, Duration::ZERO, Duration::ZERO);
    let runtime =
        RuntimeHandle::new(RuntimeConfig::new(Some(1)).with_signal_delivery(signal_delivery))?;
    let pid = runtime.spawn_test_process()?;
    runtime.terminate_test_process_with_error(pid)?;

    let error = runtime
        .deliver_signal_received(pid)
        .err()
        .ok_or("dead process delivery unexpectedly succeeded")?;

    assert!(matches!(error, EngineError::Runtime { .. }));
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn duplicate_nif_mfa_returns_typed_error() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    let mfa = Mfa::new("host", "answer", 0);
    let mut registration = NifRegistration::new();
    registration.add_host_nifs([
        NifEntry::new(mfa.clone(), forty_two),
        NifEntry::dirty(mfa, thirteen),
    ]);

    let error = runtime.install_nifs(registration).err();

    assert!(matches!(
        error,
        Some(EngineError::NifRegistration { reason })
            if reason.contains("host:answer/0")
    ));
    assert_eq!(runtime.registered_nif_modules(), vec!["host"]);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn payload_binary_remains_valid_through_spawn_and_is_released()
-> Result<(), Box<dyn std::error::Error>> {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    let mfa = Mfa::new("host", "binary_length", 1);
    let mut registration = NifRegistration::new();
    registration.add_host_nifs([NifEntry::new(mfa, binary_length)]);
    runtime.install_nifs(registration)?;

    let native_entry = runtime.lookup_native_for_test("host", "binary_length", 1);
    let module = native_call_module_with_arity_for_test(
        runtime.atom_table.intern("payload_echo"),
        runtime.atom_table.intern("run"),
        runtime.atom_table.intern("host"),
        runtime.atom_table.intern("binary_length"),
        1,
        native_entry,
    );
    runtime.module_registry.insert(module);
    let payload = Payload::new(
        aion_core::ContentType::Json,
        br#"{"hello":"world"}"#.to_vec(),
    );

    let pid =
        runtime.spawn_workflow("payload_echo", "run", RuntimeInput::from_payload(&payload)?)?;
    assert_eq!(runtime.retained_spawn_heap_count_for_test(), 1);
    let (reason, result) = runtime.run_until_exit_for_test(pid);

    assert_eq!(reason, beamr::process::ExitReason::Normal);
    assert_eq!(
        result.as_small_int(),
        Some(i64::try_from(payload.bytes().len()).unwrap_or(0))
    );
    assert_eq!(runtime.retained_spawn_heap_count_for_test(), 0);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn workflow_outcome_releases_payload_heaps() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    let mfa = Mfa::new("host", "binary_length", 1);
    let mut registration = NifRegistration::new();
    registration.add_host_nifs([NifEntry::new(mfa, binary_length)]);
    runtime.install_nifs(registration)?;

    let native_entry = runtime.lookup_native_for_test("host", "binary_length", 1);
    let module = native_call_module_with_arity_for_test(
        runtime.atom_table.intern("payload_workflow_outcome"),
        runtime.atom_table.intern("run"),
        runtime.atom_table.intern("host"),
        runtime.atom_table.intern("binary_length"),
        1,
        native_entry,
    );
    runtime.module_registry.insert(module);
    let payload = Payload::new(
        aion_core::ContentType::Json,
        br#"{"workflow":"outcome"}"#.to_vec(),
    );

    let pid = runtime.spawn_workflow(
        "payload_workflow_outcome",
        "run",
        RuntimeInput::from_payload(&payload)?,
    )?;
    assert_eq!(runtime.retained_spawn_heap_count_for_test(), 1);
    let outcome = runtime.workflow_outcome(pid)?;

    assert_eq!(
        outcome?,
        Payload::from_json(&serde_json::json!(payload.bytes().len()))?
    );
    assert_eq!(runtime.retained_spawn_heap_count_for_test(), 0);
    runtime.shutdown()?;
    Ok(())
}

#[test]
fn repeated_completed_payload_spawns_do_not_accumulate_retained_heaps()
-> Result<(), Box<dyn std::error::Error>> {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    let mfa = Mfa::new("host", "binary_length", 1);
    let mut registration = NifRegistration::new();
    registration.add_host_nifs([NifEntry::new(mfa, binary_length)]);
    runtime.install_nifs(registration)?;

    let native_entry = runtime.lookup_native_for_test("host", "binary_length", 1);
    let module = native_call_module_with_arity_for_test(
        runtime.atom_table.intern("payload_echo_many"),
        runtime.atom_table.intern("run"),
        runtime.atom_table.intern("host"),
        runtime.atom_table.intern("binary_length"),
        1,
        native_entry,
    );
    runtime.module_registry.insert(module);
    let payload = Payload::new(
        aion_core::ContentType::Json,
        br#"{"iteration":true}"#.to_vec(),
    );

    for _ in 0..1_000 {
        let pid = runtime.spawn_workflow(
            "payload_echo_many",
            "run",
            RuntimeInput::from_payload(&payload)?,
        )?;
        let (reason, result) = runtime.run_until_exit_for_test(pid);
        assert_eq!(reason, beamr::process::ExitReason::Normal);
        assert_eq!(
            result.as_small_int(),
            Some(i64::try_from(payload.bytes().len()).unwrap_or(0))
        );
        assert_eq!(runtime.retained_spawn_heap_count_for_test(), 0);
    }

    runtime.shutdown()?;
    Ok(())
}

#[test]
fn distinct_nifs_are_registered_and_callable() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    let mut registration = NifRegistration::new();
    registration.add_engine_nifs().add_host_nifs([
        NifEntry::new(Mfa::new("host", "answer", 0), forty_two),
        NifEntry::dirty(Mfa::new("host", "thirteen", 0), thirteen),
    ]);

    runtime.install_nifs(registration)?;

    assert_eq!(
        runtime.registered_nif_modules(),
        vec!["aion_flow_ffi", "host"]
    );
    let answer = runtime.lookup_native_for_test("host", "answer", 0);
    assert!(answer.is_some());
    assert!(
        runtime
            .lookup_native_for_test("host", "thirteen", 0)
            .is_some_and(|entry| entry.dirty_kind.is_some())
    );

    let host_nif_call = native_call_module_for_test(
        runtime.atom_table.intern("host_nif_call"),
        runtime.atom_table.intern("answer"),
        runtime.atom_table.intern("host"),
        runtime.atom_table.intern("answer"),
        answer,
    );
    runtime.module_registry.insert(host_nif_call);
    let pid = runtime.spawn_workflow("host_nif_call", "answer", RuntimeInput::default())?;
    let (reason, result) = runtime.run_until_exit_for_test(pid);

    assert_eq!(reason, beamr::process::ExitReason::Normal);
    assert_eq!(result, Term::small_int(42));
    runtime.shutdown()?;
    Ok(())
}
