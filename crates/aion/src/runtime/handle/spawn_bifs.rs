//! Process BIF registration with outcome tracking for locally spawned children.

use beamr::atom::AtomTable;
use beamr::native::process_bifs;
use beamr::native::{
    BifRegistryImpl, Capability, NativeEntry, NativeFn, NativeRegistrationError,
    NativeReplacementError, ProcessContext,
};
use beamr::term::Term;
use beamr::term::boxed::Tuple;
use beamr::term::pid_ref::PidRef;

use crate::EngineError;
use crate::runtime::nif_state::EngineNifState;
use crate::runtime::nif_state::engine_nif_state;

pub(super) fn register_process_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
) -> Result<(), NativeRegistrationError> {
    let erlang = atom_table.intern("erlang");
    let entries: &[(&str, u8, Capability, NativeFn)] = &[
        ("self", 0, Capability::Pure, process_bifs::bif_self),
        ("spawn", 3, Capability::Spawn, tracked_spawn),
        ("spawn", 4, Capability::Spawn, process_bifs::bif_spawn_4),
        ("spawn_link", 3, Capability::Spawn, tracked_spawn_link),
        (
            "spawn_link",
            4,
            Capability::Spawn,
            process_bifs::bif_spawn_link_4,
        ),
        (
            "spawn_monitor",
            1,
            Capability::Spawn,
            tracked_spawn_monitor_1,
        ),
        (
            "spawn_monitor",
            3,
            Capability::Spawn,
            tracked_spawn_monitor_3,
        ),
        (
            "spawn_monitor",
            4,
            Capability::Spawn,
            process_bifs::bif_spawn_monitor_4,
        ),
        ("spawn_opt", 2, Capability::Spawn, tracked_spawn_opt_2),
        ("spawn_opt", 4, Capability::Spawn, tracked_spawn_opt_4),
        ("link", 1, Capability::ProcessLocal, process_bifs::bif_link),
        (
            "unlink",
            1,
            Capability::ProcessLocal,
            process_bifs::bif_unlink,
        ),
        (
            "process_flag",
            2,
            Capability::ProcessLocal,
            process_bifs::bif_process_flag,
        ),
        (
            "monitor",
            2,
            Capability::ProcessLocal,
            process_bifs::bif_monitor,
        ),
        (
            "demonitor",
            1,
            Capability::ProcessLocal,
            process_bifs::bif_demonitor,
        ),
        (
            "exit",
            1,
            Capability::ProcessLocal,
            process_bifs::bif_exit_1,
        ),
        ("exit", 2, Capability::ProcessLocal, process_bifs::bif_exit),
    ];
    for &(name, arity, capability, native) in entries {
        registry.register(erlang, atom_table.intern(name), arity, native, capability)?;
    }
    Ok(())
}

pub(super) fn replace_gate3_fun_spawn_bifs(
    registry: &BifRegistryImpl,
    atom_table: &AtomTable,
    nif_state: &EngineNifState,
) -> Result<(), EngineError> {
    let erlang = atom_table.intern("erlang");
    let spawn =
        replace_gate3_fun_spawn_bif(registry, erlang, atom_table, "spawn", tracked_spawn_1)?;
    let spawn_link = replace_gate3_fun_spawn_bif(
        registry,
        erlang,
        atom_table,
        "spawn_link",
        tracked_spawn_link_1,
    )?;
    nif_state.set_gate3_fun_spawn_delegates(spawn, spawn_link)
}

fn replace_gate3_fun_spawn_bif(
    registry: &BifRegistryImpl,
    erlang: beamr::atom::Atom,
    atom_table: &AtomTable,
    function_name: &str,
    wrapper: NativeFn,
) -> Result<NativeEntry, EngineError> {
    let function = atom_table.intern(function_name);
    registry
        .replace_existing(erlang, function, 1, wrapper, Capability::Spawn)
        .map_err(|error| match error {
            NativeReplacementError::MissingMfa { arity, .. } => {
                EngineError::Gate3BifReplacementMissing {
                    module: String::from("erlang"),
                    function: String::from(function_name),
                    arity,
                }
            }
        })
}

fn tracked_spawn_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let state = engine_nif_state(context).map_err(|_| Term::NIL)?;
    let previous = state.gate3_spawn_delegate().ok_or(Term::NIL)?;
    invoke_tracked_spawn(args, context, previous.function)
}

fn tracked_spawn_link_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    let state = engine_nif_state(context).map_err(|_| Term::NIL)?;
    let previous = state.gate3_spawn_link_delegate().ok_or(Term::NIL)?;
    invoke_tracked_spawn(args, context, previous.function)
}

fn tracked_spawn(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    invoke_tracked_spawn(args, context, process_bifs::bif_spawn)
}

fn tracked_spawn_link(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    invoke_tracked_spawn(args, context, process_bifs::bif_spawn_link)
}

fn tracked_spawn_monitor_1(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    invoke_tracked_spawn(args, context, process_bifs::bif_spawn_monitor_1)
}

fn tracked_spawn_monitor_3(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    invoke_tracked_spawn(args, context, process_bifs::bif_spawn_monitor_3)
}

fn tracked_spawn_opt_2(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    invoke_tracked_spawn(args, context, process_bifs::bif_spawn_opt_2)
}

fn tracked_spawn_opt_4(args: &[Term], context: &mut ProcessContext) -> Result<Term, Term> {
    invoke_tracked_spawn(args, context, process_bifs::bif_spawn_opt_4)
}

fn invoke_tracked_spawn(
    args: &[Term],
    context: &mut ProcessContext,
    native: NativeFn,
) -> Result<Term, Term> {
    let state = engine_nif_state(context).map_err(|_| Term::NIL)?;
    let registry = state.process_exit_registry().ok_or(Term::NIL)?;
    let reservation = registry.reserve_spawn().map_err(|_| Term::NIL)?;
    let result = native(args, context)?;
    let pid = local_spawn_pid(result).ok_or(Term::NIL)?;
    reservation.track_unobserved(pid);
    Ok(result)
}

fn local_spawn_pid(result: Term) -> Option<u64> {
    match PidRef::new(result) {
        Some(PidRef::Local(pid)) => Some(pid),
        Some(PidRef::Remote(_)) | None => Tuple::new(result)
            .and_then(|tuple| tuple.get(0))
            .and_then(PidRef::new)
            .and_then(|pid| match pid {
                PidRef::Local(pid) => Some(pid),
                PidRef::Remote(_) => None,
            }),
    }
}

#[cfg(test)]
mod tests {
    use beamr::native::{BifRegistryImpl, NativeFn};

    use super::{replace_gate3_fun_spawn_bif, tracked_spawn_1, tracked_spawn_link_1};
    use crate::EngineError;

    #[test]
    fn missing_fun_spawn_mfas_are_typed_and_leave_vacant_keys_unchanged() {
        let atoms = beamr::atom::AtomTable::with_common_atoms();
        let erlang = atoms.intern("erlang");
        let cases: [(&str, NativeFn); 2] = [
            ("spawn", tracked_spawn_1),
            ("spawn_link", tracked_spawn_link_1),
        ];

        for (function_name, wrapper) in cases {
            let registry = BifRegistryImpl::new();
            let result =
                replace_gate3_fun_spawn_bif(&registry, erlang, &atoms, function_name, wrapper);
            assert!(matches!(
                result,
                Err(EngineError::Gate3BifReplacementMissing {
                    ref module,
                    ref function,
                    arity: 1,
                }) if module == "erlang" && function == function_name
            ));
            assert!(
                registry
                    .lookup(erlang, atoms.intern(function_name), 1)
                    .is_none(),
                "missing `{function_name}/1` replacement inserted a registry entry"
            );
        }
    }
}
