//! BC-2b-5 S6: the FIRST runtime execution proof for the direct compile
//! path. Direct-`select`ed BEAM modules are loaded into an embedded beamr VM
//! alongside the reference-emitted Gleam modules (compiled by real
//! `gleam build`), and small exported driver functions EXECUTE the generated
//! codec functions on representative values: encode, decode back, compare —
//! and the JSON bytes must be identical between the direct path and the
//! reference emitter's codecs for the same fixture types.
//!
//! Shapes covered (bounded to codec functions — whole-workflow execution
//! stays BC-4): record with an optional field, nested list-in-record, enum,
//! nested enum-in-record, union with payload, the whole-value nullable
//! composite trio (`Some`/`None` through `option_string_codec` —
//! `json.nullable`/`decode.optional`), plus the D4/S2 failure paths
//! (explicit `null` fails, absence is `None`, unknown enum/union values
//! FAIL). A final case executes the `AssertList` failure path and pins the
//! badmatch SUBJECT (carried fix b).

#[path = "runtime_codecs/child_envelope.rs"]
mod child_envelope;
#[path = "runtime_codecs/drivers.rs"]
mod drivers;
#[path = "runtime_codecs/harness.rs"]
mod harness;

use std::error::Error;

use beamr::process::ExitReason;

use aion_awl::mir::{
    Block, FlowFn, FnOrigin, FnRef, MirFn, MirModule, Span, Stmt, Tail, TyDesc, Value, select,
};

use drivers::{
    Body, collection_predicate_drivers, lowered, note_drivers, production_execute_driver,
    triage_drivers,
};
use harness::{
    build_vm, gleam_build, reference_module, workflow_id_error_ebin, workflow_id_poison_ebin,
};

type TestResult = Result<(), Box<dyn Error>>;

fn sp() -> Span {
    Span { line: 0, column: 0 }
}

// ---- the proof -------------------------------------------------------------

const NOTE_SOME_JSON: &str = r#"{"title":"t","body":"b","tags":["x","y"]}"#;
const NOTE_NONE_JSON: &str = r#"{"title":"t","tags":[]}"#;
const OPTION_SOME_JSON: &str = r#""b""#;
const OPTION_NONE_JSON: &str = "null";
const VERDICT_JSON: &str = r#"{"category":"Urgent","note":"check"}"#;
const UNION_JSON: &str = r#"{"outcome":"triaged","payload":{"category":"Urgent","note":"check"}}"#;

/// The reference-side Gleam driver appended to the emitted
/// `optional_shorthand` module.
const REF_NOTE_DRIVER: &str = r#"
pub fn awl_rt_note_some() -> #(String, Bool) {
  let value = Note(title: "t", body: Some("b"), tags: ["x", "y"])
  let c = note_codec()
  let s = c.encode(value)
  #(s, c.decode(s) == Ok(value))
}

pub fn awl_rt_note_none() -> #(String, Bool) {
  let value = Note(title: "t", body: None, tags: [])
  let c = note_codec()
  let s = c.encode(value)
  #(s, c.decode(s) == Ok(value))
}

pub fn awl_rt_note_null_fails() -> Bool {
  let c = note_codec()
  case c.decode("{\"title\":\"t\",\"body\":null,\"tags\":[]}") {
    Ok(_) -> False
    Error(_) -> True
  }
}

pub fn awl_rt_note_absent_is_none() -> Bool {
  let c = note_codec()
  c.decode("{\"title\":\"t\",\"tags\":[]}") == Ok(Note(title: "t", body: None, tags: []))
}

pub fn awl_rt_option_some() -> #(String, Bool) {
  let value = Some("b")
  let c = option_string_codec()
  let s = c.encode(value)
  #(s, c.decode(s) == Ok(value))
}

pub fn awl_rt_option_none() -> #(String, Bool) {
  let value = None
  let c = option_string_codec()
  let s = c.encode(value)
  #(s, c.decode(s) == Ok(value))
}
"#;

/// The reference-side Gleam driver appended to the emitted `triage_message`
/// module.
const REF_TRIAGE_DRIVER: &str = r#"
pub fn awl_rt_verdict() -> #(String, Bool) {
  let value = Verdict(category: Urgent, note: "check")
  let c = verdict_codec()
  let s = c.encode(value)
  #(s, c.decode(s) == Ok(value))
}

pub fn awl_rt_union() -> #(String, Bool) {
  let value = TriagedOutcome(Verdict(category: Urgent, note: "check"))
  let c = triage_message_outcome_codec()
  let s = c.encode(value)
  #(s, c.decode(s) == Ok(value))
}

pub fn awl_rt_category_unknown_fails() -> Bool {
  let c = category_codec()
  case c.decode("\"Bogus\"") {
    Ok(_) -> False
    Error(_) -> True
  }
}

pub fn awl_rt_union_unknown_fails() -> Bool {
  let c = triage_message_outcome_codec()
  case c.decode("{\"outcome\":\"bogus\",\"payload\":{}}") {
    Ok(_) -> False
    Error(_) -> True
  }
}
"#;

/// A reference-side driver over the exact record shapes and nested predicates
/// used by the collection-predicate fixture.
const REF_COLLECTION_PREDICATE_DRIVER: &str = r#"
fn findings() -> List(Finding) {
  let blocking = Finding(severity: "blocker", blocking: True)
  let non_blocking = Finding(severity: "warning", blocking: False)
  [blocking, non_blocking]
}

pub fn awl_rt_any() -> Bool {
  list.any(findings(), fn(finding) {
    finding.blocking && finding.severity == "blocker"
  })
}

pub fn awl_rt_all() -> Bool {
  list.all(findings(), fn(finding) {
    !finding.blocking || finding.severity == "blocker"
  })
}

pub fn awl_rt_nested_any() -> Bool {
  let blocking = Finding(severity: "blocker", blocking: True)
  let non_blocking = Finding(severity: "warning", blocking: False)
  let rounds = [Round(findings: [non_blocking, blocking])]
  list.any(rounds, fn(round) {
    list.any(round.findings, fn(finding) {
      finding.severity == "blocker"
    })
  })
}

pub fn awl_rt_empty_all() -> Bool {
  let empty: List(Finding) = []
  list.all(empty, fn(finding) { finding.blocking })
}

pub fn awl_rt_empty_any() -> Bool {
  let empty: List(Finding) = []
  list.any(empty, fn(finding) { finding.blocking })
}
"#;

const REF_EXECUTE_BOTH: &str = r#"
pub fn awl_rt_execute() {
  execute(FallibleCollectionPredicatesInput(findings: [Finding(owner_id: "test-workflow-id")]))
}
"#;

const REF_EXECUTE_ANY: &str = r#"
pub fn awl_rt_execute() {
  execute(FallibleAnyShortCircuitInput(findings: [Finding(owner_id: "test-workflow-id"), Finding(owner_id: "poison")]))
}
"#;

const REF_EXECUTE_ALL: &str = r#"
pub fn awl_rt_execute() {
  execute(FallibleAllShortCircuitInput(findings: [Finding(owner_id: "other"), Finding(owner_id: "poison")]))
}
"#;

#[test]
fn nested_collection_predicates_execute_with_reference_parity() -> TestResult {
    let reference = reference_module(
        "step-bodies/valid/collection_predicates.awl",
        REF_COLLECTION_PREDICATE_DRIVER,
    )?;
    let ebins = gleam_build(&[("ref_collection_predicates", &reference)])?;

    let mut direct = lowered("step-bodies/valid/collection_predicates.awl")?;
    collection_predicate_drivers(&mut direct)?;
    let direct_bytes = select(&direct)?;
    let vm = build_vm(&ebins, &[direct_bytes])?;

    let cases = [
        ("any", "true"),
        ("all", "true"),
        ("nested_any", "true"),
        ("empty_all", "true"),
        ("empty_any", "false"),
    ];
    for (name, expected) in cases {
        let direct_result = vm.call0("collection_predicates", &format!("awl$rt_{name}"))?;
        let reference_result = vm.call0("ref_collection_predicates", &format!("awl_rt_{name}"))?;
        assert_eq!(direct_result, reference_result, "{name} parity");
        assert_eq!(direct_result, expected, "{name} semantics");
    }
    Ok(())
}

#[test]
fn fallible_predicates_execute_through_production_hosts_with_parity() -> TestResult {
    let success_reference = reference_module(
        "step-bodies/valid/fallible_collection_predicates.awl",
        REF_EXECUTE_BOTH,
    )?;
    let mut direct = lowered("step-bodies/valid/fallible_collection_predicates.awl")?;
    production_execute_driver(&mut direct, &["test-workflow-id"]);
    let direct_bytes = select(&direct)?;
    let success_ebins = gleam_build(&[("ref_fallible_collection_predicates", &success_reference)])?;
    let vm = build_vm(&success_ebins, std::slice::from_ref(&direct_bytes))?;
    let direct_success = vm.call0("fallible_collection_predicates", "awl$rt_execute")?;
    let reference_success = vm.call0("ref_fallible_collection_predicates", "awl_rt_execute")?;
    assert_eq!(
        direct_success, reference_success,
        "successful execute parity"
    );
    assert!(direct_success.starts_with("{ok,"));

    let mut error_ebins = success_ebins;
    error_ebins.push(workflow_id_error_ebin()?);
    let vm = build_vm(&error_ebins, &[direct_bytes])?;
    let direct_error = vm.call0("fallible_collection_predicates", "awl$rt_execute")?;
    let reference_error = vm.call0("ref_fallible_collection_predicates", "awl_rt_execute")?;
    assert_eq!(direct_error, reference_error, "execute error parity");
    assert_eq!(direct_error, "{error, awl_failed}");

    let any_reference = reference_module(
        "step-bodies/valid/fallible_any_short_circuit.awl",
        REF_EXECUTE_ANY,
    )?;
    let all_reference = reference_module(
        "step-bodies/valid/fallible_all_short_circuit.awl",
        REF_EXECUTE_ALL,
    )?;
    let ebins = gleam_build(&[
        ("ref_fallible_any_short_circuit", &any_reference),
        ("ref_fallible_all_short_circuit", &all_reference),
    ])?;
    let mut any_direct = lowered("step-bodies/valid/fallible_any_short_circuit.awl")?;
    production_execute_driver(&mut any_direct, &["test-workflow-id", "poison"]);
    let mut all_direct = lowered("step-bodies/valid/fallible_all_short_circuit.awl")?;
    production_execute_driver(&mut all_direct, &["other", "poison"]);
    let direct_modules = [select(&any_direct)?, select(&all_direct)?];

    let mut forced_error_ebins = ebins.clone();
    forced_error_ebins.push(workflow_id_error_ebin()?);
    let vm = build_vm(&forced_error_ebins, &direct_modules)?;
    for module in ["fallible_any_short_circuit", "fallible_all_short_circuit"] {
        let direct_result = vm.call0(module, "awl$rt_execute")?;
        let reference_result = vm.call0(&format!("ref_{module}"), "awl_rt_execute")?;
        assert_eq!(direct_result, reference_result, "{module} error parity");
        assert_eq!(direct_result, "{error, awl_failed}");
    }

    let mut poisoned_ebins = ebins;
    poisoned_ebins.push(workflow_id_poison_ebin()?);
    let vm = build_vm(&poisoned_ebins, &direct_modules)?;
    for (module, expected) in [
        ("fallible_any_short_circuit", "true"),
        ("fallible_all_short_circuit", "false"),
    ] {
        let direct_result = vm.call0(module, "awl$rt_execute")?;
        let reference_result = vm.call0(&format!("ref_{module}"), "awl_rt_execute")?;
        assert_eq!(direct_result, reference_result, "{module} parity");
        assert!(
            direct_result.contains(expected),
            "{module}: {direct_result}"
        );
    }
    Ok(())
}

/// The full differential runtime proof: direct-selected codec functions and
/// reference-emitted (gleam-built) codec functions execute on ONE beamr VM;
/// their JSON bytes must match each other and the pinned expected bytes, and
/// every round-trip/failure semantic must hold on BOTH sides.
#[test]
fn direct_codecs_execute_with_reference_parity() -> TestResult {
    // Reference side: emit + append Gleam drivers + real gleam build.
    let ref_note = reference_module("schema-doors/valid/optional_shorthand.awl", REF_NOTE_DRIVER)?;
    let ref_triage = reference_module("header-types/valid/enum.awl", REF_TRIAGE_DRIVER)?;
    let ebins = gleam_build(&[
        ("ref_optional_shorthand", &ref_note),
        ("ref_triage_message", &ref_triage),
    ])?;

    // Direct side: lower + append MIR drivers + select.
    let mut note_module = lowered("schema-doors/valid/optional_shorthand.awl")?;
    note_drivers(&mut note_module)?;
    let note_bytes = select(&note_module)?;
    let mut triage_module = lowered("header-types/valid/enum.awl")?;
    triage_drivers(&mut triage_module)?;
    let triage_bytes = select(&triage_module)?;

    let vm = build_vm(&ebins, &[note_bytes, triage_bytes])?;

    // Round trips: direct bytes == reference bytes == pinned JSON, and both
    // sides decode their own encoding back to the original value.
    let cases: &[(&str, &str, &str, &str, &str)] = &[
        (
            "optional_shorthand",
            "awl$rt_note_some",
            "ref_optional_shorthand",
            "awl_rt_note_some",
            NOTE_SOME_JSON,
        ),
        (
            "optional_shorthand",
            "awl$rt_note_none",
            "ref_optional_shorthand",
            "awl_rt_note_none",
            NOTE_NONE_JSON,
        ),
        (
            "optional_shorthand",
            "awl$rt_option_some",
            "ref_optional_shorthand",
            "awl_rt_option_some",
            OPTION_SOME_JSON,
        ),
        (
            "optional_shorthand",
            "awl$rt_option_none",
            "ref_optional_shorthand",
            "awl_rt_option_none",
            OPTION_NONE_JSON,
        ),
        (
            "triage_message",
            "awl$rt_verdict",
            "ref_triage_message",
            "awl_rt_verdict",
            VERDICT_JSON,
        ),
        (
            "triage_message",
            "awl$rt_union",
            "ref_triage_message",
            "awl_rt_union",
            UNION_JSON,
        ),
    ];
    for (direct_module, direct_fn, ref_module, ref_fn, expected) in cases {
        let (direct_json, direct_equal) = vm.roundtrip(direct_module, direct_fn)?;
        let (ref_json, ref_equal) = vm.roundtrip(ref_module, ref_fn)?;
        assert_eq!(
            direct_json, *expected,
            "direct {direct_fn} JSON bytes diverge from the pinned wire form"
        );
        assert_eq!(
            direct_json, ref_json,
            "direct {direct_fn} JSON bytes diverge from the reference emitter's codec output"
        );
        assert!(direct_equal, "direct {direct_fn} did not round-trip");
        assert!(ref_equal, "reference {ref_fn} did not round-trip");
    }

    // Failure semantics (S2/D4), on BOTH sides: explicit null fails, unknown
    // enum variants fail, unknown union outcomes fail; absence is None.
    let boolean_cases: &[(&str, &str)] = &[
        ("optional_shorthand", "awl$rt_note_null_fails"),
        ("optional_shorthand", "awl$rt_note_absent_is_none"),
        ("triage_message", "awl$rt_category_unknown_fails"),
        ("triage_message", "awl$rt_union_unknown_fails"),
        ("ref_optional_shorthand", "awl_rt_note_null_fails"),
        ("ref_optional_shorthand", "awl_rt_note_absent_is_none"),
        ("ref_triage_message", "awl_rt_category_unknown_fails"),
        ("ref_triage_message", "awl_rt_union_unknown_fails"),
    ];
    for (module, function) in boolean_cases {
        let formatted = vm.call0(module, function)?;
        assert_eq!(
            formatted, "true",
            "{module}:{function}/0 semantic check failed"
        );
    }
    Ok(())
}

/// Carried fix (b) EXECUTED: an overlong `let assert [a, b] = subject`
/// badmatch-traps with the SUBJECT list `[1, 2, 3]` — not the walked tail
/// `[3]` the pre-fix operand reported.
#[test]
fn assert_list_badmatch_reports_the_subject_at_runtime() -> TestResult {
    let mut body = Body::new();
    let subject = body.list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    let first = body.var();
    let second = body.var();
    body.stmts.push(Stmt::AssertList {
        binds: vec![Some(first), Some(second)],
        list: subject,
        span: sp(),
    });
    let module = MirModule {
        name: "awl_rt_assert_list".to_owned(),
        source: "awl_rt_assert_list.awl".to_owned(),
        atoms: Vec::new(),
        literals: Vec::new(),
        exports: vec![FnRef(0)],
        functions: vec![MirFn::Flow(FlowFn {
            // `select` locates the module's Execute entry (`find_execute`),
            // so the single hand-built fn carries that origin.
            origin: FnOrigin::Execute,
            name: "overlong".to_owned(),
            params: Vec::new(),
            param_tys: Vec::new(),
            ret_ty: TyDesc::Nil,
            body: Block {
                stmts: body.stmts,
                tail: Tail::Return(Value::Var(first)),
            },
            span: sp(),
            degraded_parallel: false,
        })],
        types: Vec::new(),
    };
    let bytes = select(&module)?;
    let vm = build_vm(&[], &[bytes])?;
    let (reason, formatted, _) = vm.call0_raw("awl_rt_assert_list", "overlong")?;
    assert_ne!(
        reason,
        ExitReason::Normal,
        "the overlong destructure did not trap: {formatted}"
    );
    assert!(
        formatted.contains("[1, 2, 3]"),
        "badmatch did not report the SUBJECT list: {formatted}"
    );
    assert!(
        !formatted.contains("badmatch: [3]"),
        "badmatch reported the walked tail, not the subject: {formatted}"
    );
    Ok(())
}
