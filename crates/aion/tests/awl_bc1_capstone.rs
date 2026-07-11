//! AWL BC-1 capstone — the ratified evidence gate before BC-2 (D-BC5).
//!
//! Runs ONLY in the capstone worktree: the workspace `[patch.crates-io]`
//! points beamr at the BC-1 encode worktree so the `encode` feature (the
//! `.beam` container writer) is available. This file and the patch never
//! merge to aion main; the trail normalizer in `common/trail_norm.rs` is the
//! seed of the BC-4 differential harness (plan §3).
//!
//! Deliverable A — re-encoded module through the full production path: the
//! Gleam-built `awl_hello` example is decoded with beamr's loader, re-encoded
//! with the BC-1 writer, repackaged with its untouched-vs-also-re-encoded SDK
//! `.beam` closure, loaded through the catalog (exercising the content-hash
//! rename machinery under a DIFFERENT hash than the original bytes), and run
//! end-to-end against a local activity dispatcher. The durable event trail
//! must be identical to the original package's trail after normalization.
//!
//! Deliverable B — a genuinely hand-CONSTRUCTED module: `run/1` is built
//! instruction-by-instruction in Rust against the `ParsedModule` model (no
//! Gleam, no erlc anywhere in its production), encoded with the BC-1 writer,
//! explicitly validated, then packaged with the SDK closure and run through
//! the real engine. It calls into `aion_flow` (`aion@duration:milliseconds/1`
//! then `aion@workflow:sleep/1` — a durable timer) and completes a run whose
//! normalized trail must match its Gleam-built twin
//! (`tests/fixtures/capstone_twin`).

#[path = "common/example_build.rs"]
mod example_build;
#[path = "common/trail_norm.rs"]
mod trail_norm;

use std::collections::HashMap;
use std::sync::Arc;

use aion::EngineBuilder;
use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use aion_core::{Event, Payload};
use aion_package::{BeamModule, BeamSet, ExtractionLimits, Package, PackageBuilder};
use aion_store::{EventStore, InMemoryStore};
use beamr::atom::AtomTable;
use beamr::loader::decode::{Instruction, Literal, Operand};
use beamr::loader::encode::encode_module;
use beamr::loader::load::{ParsedModule, resolve_imports};
use beamr::loader::validate::validate_module;
use beamr::loader::{ExportEntry, ImportEntry, load_beam_chunks};
use beamr::module::ModuleRegistry;
use beamr::native::{AllCapabilitiesPolicy, BifRegistry, NativeEntry};
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Deterministic local activity harness for the `awl_hello` workflow's two
/// activities. Pure functions of their inputs, so the original and
/// re-encoded runs must record byte-identical activity payloads.
struct CapstoneDispatcher;

impl ActivityDispatcher for CapstoneDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        let input: serde_json::Value = serde_json::from_str(request.input.as_str())
            .map_err(|error| format!("terminal:bad input: {error}"))?;
        match request.name.as_str() {
            "greet" => {
                let name = input["name"].as_str().unwrap_or("stranger");
                Ok(json!({ "greeting": format!("Hello, {name}!") }).to_string())
            }
            "shout" => {
                let text = input["text"].as_str().unwrap_or("");
                Ok(json!({ "text": format!("{}!!", text.to_uppercase()) }).to_string())
            }
            other => Err(format!("terminal:unknown activity {other}")),
        }
    }
}

/// Empty BIF registry for standalone `validate_module` runs (imports resolve
/// to deferred targets, which validation accepts).
struct NoBifs;

impl BifRegistry for NoBifs {
    fn lookup(
        &self,
        _module: beamr::atom::Atom,
        _function: beamr::atom::Atom,
        _arity: u8,
    ) -> Option<NativeEntry> {
        None
    }
}

/// Runs one package through a fresh real engine (catalog load → content-hash
/// rename registration → entry dispatch → durable recording) and returns the
/// full durable event trail plus the workflow result payload bytes.
async fn run_package(
    package: Package,
    workflow_type: &str,
    input: &serde_json::Value,
) -> Result<(Vec<Event>, Vec<u8>), Box<dyn std::error::Error>> {
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = EngineBuilder::new()
        .store_arc(Arc::clone(&store))
        .in_memory_visibility()
        .scheduler_threads(1)
        .activity_dispatcher(Arc::new(CapstoneDispatcher))
        .load_workflows(package)
        .build()
        .await?;
    let handle = engine
        .start_workflow(
            workflow_type,
            Payload::from_json(input)?,
            HashMap::new(),
            String::from("default"),
        )
        .await?;
    let result = engine.result(handle.workflow_id(), handle.run_id()).await?;
    let payload = result.map_err(|error| format!("workflow failed: {error:?}"))?;
    let history = store.read_history(handle.workflow_id()).await?;
    engine.shutdown()?;
    Ok((history, payload.bytes().to_vec()))
}

/// Decodes every module in `package` with beamr's loader, re-encodes it with
/// the BC-1 writer, proves the re-encoded bytes decode back to the identical
/// `ParsedModule`, and returns a new package rebuilt from the re-encoded
/// bytes through the production `PackageBuilder` path (fresh content hash,
/// fresh deployed names).
fn reencode_package(package: &Package) -> Result<Package, Box<dyn std::error::Error>> {
    let table = AtomTable::with_common_atoms();
    let mut modules = Vec::new();
    for module in package.beams().iter() {
        let original = load_beam_chunks(module.bytes(), &table)
            .map_err(|error| format!("`{}` does not decode: {error}", module.name()))?;
        let bytes = encode_module(&original, &table)
            .map_err(|error| format!("`{}` does not re-encode: {error}", module.name()))?;
        let reloaded = load_beam_chunks(&bytes, &table)
            .map_err(|error| format!("re-encoded `{}` does not decode: {error}", module.name()))?;
        assert_eq!(
            original,
            reloaded,
            "round-trip mismatch in `{}`",
            module.name()
        );
        assert_ne!(
            module.bytes(),
            bytes.as_slice(),
            "`{}` re-encoded to the original erlc bytes — the writer's canonical \
             chunk set differs from erlc's, so identical bytes indicate the \
             original bytes were passed through untouched",
            module.name()
        );
        modules.push(BeamModule::new(module.name(), bytes));
    }
    let archive =
        PackageBuilder::new(package.manifest().clone(), BeamSet::new(modules)?).write_to_bytes()?;
    Ok(Package::load_from_bytes(
        archive,
        ExtractionLimits::unbounded(),
    )?)
}

/// Deliverable A: the re-encoded `awl_hello` package (entry module AND its
/// entire SDK `.beam` closure rewritten by the BC-1 writer) completes the
/// same run through the real engine with a durable trail identical to the
/// original package's after normalization.
#[tokio::test(flavor = "multi_thread")]
async fn deliverable_a_reencoded_awl_hello_matches_original_trail() -> TestResult {
    let original = example_build::built_package("examples/awl-hello", "awl_hello")?;
    let reencoded = reencode_package(&original)?;

    // The re-encoded bytes hash differently, so the catalog's content-hash
    // rename machinery registers the same logical modules under different
    // deployed names — both loads exercise `register_module_with_renames`
    // with genuinely distinct rename maps.
    assert_ne!(
        original.content_hash(),
        reencoded.content_hash(),
        "re-encoded package must carry a fresh content hash"
    );
    assert_ne!(
        original.deployed_entry_module(),
        reencoded.deployed_entry_module()
    );
    println!(
        "original package : {} modules, hash {}, entry {}",
        original.beams().len(),
        original.content_hash(),
        original.deployed_entry_module()
    );
    println!(
        "re-encoded package: {} modules, hash {}, entry {}",
        reencoded.beams().len(),
        reencoded.content_hash(),
        reencoded.deployed_entry_module()
    );

    let input = json!({ "name": "Capstone" });
    let (original_trail, original_result) = run_package(original, "awl_hello", &input).await?;
    let (reencoded_trail, reencoded_result) = run_package(reencoded, "awl_hello", &input).await?;

    // Shape guard so an accidentally-empty pair of trails cannot pass.
    let completed = |events: &[Event]| {
        events
            .iter()
            .filter(|event| matches!(event, Event::ActivityCompleted { .. }))
            .count()
    };
    assert!(
        matches!(original_trail.first(), Some(Event::WorkflowStarted { .. }))
            && matches!(original_trail.last(), Some(Event::WorkflowCompleted { .. }))
            && completed(&original_trail) == 2,
        "unexpected original trail: {original_trail:#?}"
    );

    assert_eq!(
        String::from_utf8(original_result.clone())?,
        json!({ "outcome": "shouted", "payload": { "text": "HELLO, CAPSTONE!!!" } }).to_string()
    );
    assert_eq!(
        original_result, reencoded_result,
        "re-encoded run produced a different workflow result"
    );
    let normalized = trail_norm::normalized_trail(&original_trail)?;
    assert_eq!(
        normalized,
        trail_norm::normalized_trail(&reencoded_trail)?,
        "normalized durable trails diverged between the original and re-encoded bytes"
    );
    println!("=== deliverable A evidence ===");
    println!("normalized trail ({} events):", normalized.len());
    println!("{}", serde_json::to_string_pretty(&normalized)?);
    Ok(())
}

/// The atoms of the hand-built capstone module, interned once and shared
/// between the instruction stream and the module tables.
struct CapstoneAtoms {
    module_name: beamr::atom::Atom,
    run: beamr::atom::Atom,
    ok: beamr::atom::Atom,
    error: beamr::atom::Atom,
    duration_module: beamr::atom::Atom,
    milliseconds: beamr::atom::Atom,
    workflow_module: beamr::atom::Atom,
    sleep: beamr::atom::Atom,
}

impl CapstoneAtoms {
    fn interned(table: &AtomTable) -> Self {
        Self {
            module_name: table.intern("capstone_twin"),
            run: table.intern("run"),
            ok: table.intern("ok"),
            error: table.intern("error"),
            duration_module: table.intern("aion@duration"),
            milliseconds: table.intern("milliseconds"),
            workflow_module: table.intern("aion@workflow"),
            sleep: table.intern("sleep"),
        }
    }
}

/// The hand-written BEAM instruction sequence for `run/1`: allocate a bare
/// frame, two `call_ext`s into the SDK, one tagged-tuple test on the
/// `Result`, and a literal move per branch.
fn hand_built_instructions(atoms: &CapstoneAtoms) -> Vec<Instruction> {
    vec![
        Instruction::Label { label: 1 },
        Instruction::FuncInfo {
            module: Operand::Atom(Some(atoms.module_name)),
            function: Operand::Atom(Some(atoms.run)),
            arity: Operand::Unsigned(1),
        },
        Instruction::Label { label: 2 },
        // Bare stack frame for the two non-tail calls; the raw workflow input
        // in x0 is dead (the twin ignores it), so nothing is live.
        Instruction::Allocate {
            stack_need: Operand::Unsigned(0),
            live: Operand::Unsigned(0),
        },
        Instruction::Move {
            source: Operand::Integer(25),
            destination: Operand::X(0),
        },
        // aion@duration:milliseconds(25) -> Duration record in x0.
        Instruction::CallExt {
            arity: Operand::Unsigned(1),
            import: Operand::Unsigned(0),
        },
        // aion@workflow:sleep(Duration) -> {ok, nil} | {error, EngineError}.
        Instruction::CallExt {
            arity: Operand::Unsigned(1),
            import: Operand::Unsigned(1),
        },
        Instruction::IsTaggedTuple {
            fail: Operand::Label(3),
            value: Operand::X(0),
            arity: Operand::Unsigned(2),
            tag: Operand::Atom(Some(atoms.ok)),
        },
        Instruction::Move {
            source: Operand::Literal(0),
            destination: Operand::X(0),
        },
        Instruction::Deallocate {
            words: Operand::Unsigned(0),
        },
        Instruction::Return,
        Instruction::Label { label: 3 },
        Instruction::Move {
            source: Operand::Literal(1),
            destination: Operand::X(0),
        },
        Instruction::Deallocate {
            words: Operand::Unsigned(0),
        },
        Instruction::Return,
    ]
}

/// Builds the capstone module BY HAND against beamr's decoded-module model:
/// no Gleam source, no erlc, no bytes copied from any compiled artifact. The
/// exported `run/1` mirrors the twin's observable behaviour — build a
/// duration, call `aion_flow`'s durable sleep, and return
/// `{ok, <<"\"capstone\"">>}` (or `{error, <<"timer failed">>}` on the
/// untaken failure branch).
fn hand_built_capstone_module() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let table = AtomTable::with_common_atoms();
    let atoms = CapstoneAtoms::interned(&table);
    let module = ParsedModule {
        name: atoms.module_name,
        atoms: vec![
            atoms.module_name,
            atoms.run,
            atoms.ok,
            atoms.error,
            atoms.duration_module,
            atoms.milliseconds,
            atoms.workflow_module,
            atoms.sleep,
        ],
        instructions: hand_built_instructions(&atoms),
        imports: vec![
            ImportEntry {
                module: atoms.duration_module,
                function: atoms.milliseconds,
                arity: 1,
            },
            ImportEntry {
                module: atoms.workflow_module,
                function: atoms.sleep,
                arity: 1,
            },
        ],
        exports: vec![ExportEntry {
            function: atoms.run,
            arity: 1,
            label: 2,
        }],
        lambdas: Vec::new(),
        literals: vec![
            Literal::Tuple(vec![
                Literal::Atom(atoms.ok),
                Literal::Binary(b"\"capstone\"".to_vec()),
            ]),
            Literal::Tuple(vec![
                Literal::Atom(atoms.error),
                Literal::Binary(b"timer failed".to_vec()),
            ]),
        ],
        string_table: Vec::new(),
        line_info: Vec::new(),
    };

    Ok(encode_module(&module, &table)?)
}

/// Deliverable B: the hand-constructed module loads, validates, calls into
/// `aion_flow`, and completes a real engine run whose normalized durable
/// trail is identical to its Gleam-built twin's.
#[tokio::test(flavor = "multi_thread")]
async fn deliverable_b_hand_built_module_matches_gleam_twin_trail() -> TestResult {
    let twin =
        example_build::built_package("crates/aion/tests/fixtures/capstone_twin", "capstone_twin")?;
    let hand_bytes = hand_built_capstone_module()?;

    // Explicit standalone load + validate of the hand-built bytes, before the
    // engine's own five validation layers see them at catalog load.
    let table = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(&hand_bytes, &table)
        .map_err(|error| format!("hand-built module does not decode: {error}"))?;
    let registry = ModuleRegistry::new();
    let (resolved, _report) = resolve_imports(&parsed, &registry, &NoBifs, &AllCapabilitiesPolicy);
    validate_module(&parsed, &resolved)
        .map_err(|error| format!("hand-built module fails validation: {error:?}"))?;

    // Package the hand-built entry module with the twin's untouched SDK
    // `.beam` closure (aion_flow + gleam stdlib), through the production
    // builder path.
    let mut modules = Vec::new();
    for module in twin.beams().iter() {
        if module.name() == "capstone_twin" {
            modules.push(BeamModule::new(module.name(), hand_bytes.clone()));
        } else {
            modules.push(BeamModule::new(module.name(), module.bytes()));
        }
    }
    let archive =
        PackageBuilder::new(twin.manifest().clone(), BeamSet::new(modules)?).write_to_bytes()?;
    let hand_package = Package::load_from_bytes(archive, ExtractionLimits::unbounded())?;
    assert_ne!(twin.content_hash(), hand_package.content_hash());

    let twin_module_len = twin.beams().get("capstone_twin").map_or(0, <[u8]>::len);
    let input = json!("input is ignored");
    let (twin_trail, twin_result) = run_package(twin, "capstone_twin", &input).await?;
    let (hand_trail, hand_result) = run_package(hand_package, "capstone_twin", &input).await?;

    // Shape guard: the durable trail must show the aion_flow timer actually
    // ran — Started, TimerStarted, TimerFired, Completed.
    let kinds = |events: &[Event]| -> Vec<&'static str> {
        events
            .iter()
            .map(|event| match event {
                Event::WorkflowStarted { .. } => "WorkflowStarted",
                Event::TimerStarted { .. } => "TimerStarted",
                Event::TimerFired { .. } => "TimerFired",
                Event::WorkflowCompleted { .. } => "WorkflowCompleted",
                _ => "other",
            })
            .collect()
    };
    assert_eq!(
        kinds(&twin_trail),
        vec![
            "WorkflowStarted",
            "TimerStarted",
            "TimerFired",
            "WorkflowCompleted"
        ],
        "unexpected twin trail: {twin_trail:#?}"
    );

    assert_eq!(String::from_utf8(twin_result.clone())?, "\"capstone\"");
    assert_eq!(
        twin_result, hand_result,
        "hand-built run produced a different workflow result"
    );
    let normalized = trail_norm::normalized_trail(&twin_trail)?;
    assert_eq!(
        normalized,
        trail_norm::normalized_trail(&hand_trail)?,
        "normalized durable trails diverged between the Gleam twin and the hand-built module"
    );
    println!("=== deliverable B evidence ===");
    println!(
        "hand-built module: {} bytes (twin's erlc production: {twin_module_len} bytes)",
        hand_bytes.len(),
    );
    println!("normalized trail ({} events):", normalized.len());
    println!("{}", serde_json::to_string_pretty(&normalized)?);
    Ok(())
}
