//! Static lowering/bundle skew guard: every `(module, function, arity)` the
//! MIR lowering may mint (`RuntimeFn::import_surface`) must be exported by a
//! module shipped in the embedded SDK closure.
//!
//! This is the guard the `map_settled` incident demanded: the lowering grew a
//! `RuntimeFn` row (`aion@workflow:map_settled/2`) while the committed
//! closure predated the SDK function, so tolerant single-activity distribute
//! archives crashed a running VM with `undef` — a skew no compile step could
//! see. With this test, a runtime row the closure does not export fails CI
//! the moment it is introduced.

use std::collections::{BTreeMap, BTreeSet};

use aion_awl::mir::RuntimeFn;
use aion_awl_package::sdk_closure_modules;
use beamr::atom::AtomTable;
use beamr::loader::load_beam_chunks;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Modules the Erlang runtime itself provides. `RuntimeFn::IntAdd` is the one
/// BIF-position row (`erlang:'+'/2`); beamr resolves it natively, so no
/// bundle module can or should export it.
const VM_PROVIDED_MODULES: &[&str] = &["erlang"];

/// Module name → exported `(function, arity)` set.
type ExportMap = BTreeMap<String, BTreeSet<(String, u32)>>;

/// The export map of the embedded closure, read through the same beamr
/// chunk loader the engine uses.
fn closure_exports() -> Result<ExportMap, Box<dyn std::error::Error>> {
    let atoms = AtomTable::with_common_atoms();
    let mut exports = BTreeMap::new();
    for (name, bytes) in sdk_closure_modules() {
        let parsed = load_beam_chunks(bytes, &atoms)?;
        let mut set = BTreeSet::new();
        for export in &parsed.exports {
            let Some(function) = atoms.resolve(export.function) else {
                return Err(format!(
                    "module `{name}` has an unresolvable export atom: {:?}",
                    export.function
                )
                .into());
            };
            set.insert((function.to_owned(), u32::from(export.arity)));
        }
        exports.insert(name.to_owned(), set);
    }
    Ok(exports)
}

/// Every runtime signature the lowering may mint is exported by a shipped
/// closure module (VM-provided modules excepted).
#[test]
fn every_runtime_fn_signature_is_exported_by_the_closure() -> TestResult {
    let exports = closure_exports()?;
    let surface = RuntimeFn::import_surface();
    // Vacuous-pass guards: the surface and the closure must both be
    // non-trivial, and the incident row itself must be present.
    assert!(surface.len() > 50, "runtime surface shrank suspiciously");
    assert!(!exports.is_empty(), "closure export map is empty");
    assert!(
        surface.iter().any(|(module, function, arity)| {
            *module == "aion@workflow" && function == "map_settled" && *arity == 2
        }),
        "the surface no longer carries aion@workflow:map_settled/2 — update this guard's anchors"
    );

    let mut missing = Vec::new();
    for (module, function, arity) in surface {
        if VM_PROVIDED_MODULES.contains(&module) {
            continue;
        }
        let exported = exports
            .get(module)
            .is_some_and(|set| set.contains(&(function.clone(), arity)));
        if !exported {
            missing.push(format!("{module}:{function}/{arity}"));
        }
    }
    assert!(
        missing.is_empty(),
        "runtime rows the embedded SDK closure does not export (each would be an `undef` crash \
         in a deployed direct archive): {missing:?}"
    );
    Ok(())
}
