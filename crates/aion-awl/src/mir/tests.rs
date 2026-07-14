//! In-crate MIR tests (the module is crate-private, so tests live here rather
//! than in `tests/`). Golden coverage + §5 op-shape assertions.
//!
//! Goldens live under `tests/mir-goldens/`; set `AWL_BC2_BLESS=1` to (re)write
//! them. `verify` (S1) runs inside every golden. The golden set covers every
//! checking fixture this BC-2 increment fully lowers; fixtures that hit a
//! deferred shape are recorded (they must fail with `Unsupported`/`Planning`,
//! never a panic).

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use super::{LowerError, MirModule, lower, print_mir, project_sidecar, verify};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn valid_fixtures() -> Vec<PathBuf> {
    let root = manifest_dir().join("tests/fixtures/rev2");
    let mut found = Vec::new();
    collect_awl(&root, &mut found);
    found.sort();
    found
}

fn collect_awl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_awl(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "awl")
            && path.components().any(|c| c.as_os_str() == "valid")
        {
            out.push(path);
        }
    }
}

/// Read + parse a fixture, then lower it. A read or parse failure of a
/// `valid/` fixture is a hard test error (returned as the outer `Err`), never
/// a silently-counted "deferred" outcome; the inner `Result` is the lowering
/// outcome (`Ok` covered, `Err` a recorded refusal).
fn lower_fixture(path: &Path) -> Result<Result<MirModule, LowerError>, Box<dyn std::error::Error>> {
    let source = fs::read_to_string(path)?;
    let document = crate::parse(&source)
        .map_err(|error| format!("valid fixture {} no longer parses: {error}", path.display()))?;
    Ok(lower(&document, path.parent()))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The exact set of `valid/` fixtures this BC-2 increment lowers, pinned so a
/// regression from covered → refused (or a newly-covered fixture) fails the
/// ratchet instead of being silently absorbed by the deferred bucket. Paths
/// are relative to `tests/fixtures/rev2`, without the `.awl` extension.
const COVERED: &[&str] = &[
    "dag-fork/valid/after_single",
    "dag-fork/valid/child_collection_fork",
    "dag-fork/valid/child_collection_fork_sequential",
    "dag-fork/valid/fall_through_chain",
    "dag-fork/valid/fork_action_fanout",
    "dag-fork/valid/fork_named_branches",
    "dag-fork/valid/fork_named_homogeneous",
    "dag-fork/valid/fork_sequential_route",
    "dag-fork/valid/sit_one",
    "declarations/valid/worker_action_config_lines",
    "declarations/valid/worker_retry_backoff",
    "declarations/valid/worker_single_action",
    "declarations/valid/workers_multiple",
    "flagship/valid/awl_hello",
    "header-types/valid/builtins",
    "header-types/valid/doc_comments",
    "header-types/valid/enum",
    "header-types/valid/line_width",
    "header-types/valid/minimal",
    "header-types/valid/noncanonical_commas",
    "header-types/valid/workflow_timeout",
    "header-types/valid/zero_inputs",
    "loop-outcomes/valid/backward_route_bounded_cycle",
    "loop-outcomes/valid/enum_when_totality",
    "loop-outcomes/valid/float_threshold_guard",
    "loop-outcomes/valid/fork_in_loop_live_ins",
    "loop-outcomes/valid/loop_after_fall_through",
    "loop-outcomes/valid/loop_compound_until_nested",
    "loop-outcomes/valid/loop_counting_until_max",
    "loop-outcomes/valid/loop_without_counting",
    "loop-outcomes/valid/route_outcome_by_name",
    "schema-doors/valid/import_constraints",
    "schema-doors/valid/import_ticket",
    "schema-doors/valid/inline_schema_round",
    "schema-doors/valid/inline_verbatim_constraints",
    "schema-doors/valid/optional_shorthand",
    "schema-doors/valid/short_circuit_optional",
    "step-bodies/valid/calls_and_side_effects",
    "step-bodies/valid/collection_predicates",
    "step-bodies/valid/fallible_all_short_circuit",
    "step-bodies/valid/fallible_any_short_circuit",
    "step-bodies/valid/fallible_collection_predicates",
    "step-bodies/valid/general_concat",
    "step-bodies/valid/pipe_chain_stages",
    "step-bodies/valid/predicates_and_operators",
    "step-bodies/valid/workflow_id",
];

/// Compare `contents` against the on-disk golden. A MISSING golden is a hard
/// failure (a covered fixture must have a committed golden) unless
/// `AWL_BC2_BLESS` is set, in which case the golden is (re)written. Blessing
/// NEVER happens implicitly.
fn check_golden(path: &Path, contents: &str) -> Result<(), String> {
    if std::env::var("AWL_BC2_BLESS").is_ok() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        return fs::write(path, contents).map_err(|error| error.to_string());
    }
    let expected = fs::read_to_string(path).map_err(|error| {
        format!(
            "missing golden {} (a covered fixture must have a committed golden; \
             run with AWL_BC2_BLESS=1 to create): {error}",
            path.display()
        )
    })?;
    if expected == contents {
        Ok(())
    } else {
        Err(format!("golden mismatch at {}", path.display()))
    }
}

/// Every committed MIR and sidecar golden must belong to a fixture in the
/// covered set; either artifact becoming orphaned is a ratchet failure.
fn walk_goldens(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_goldens(&path, out);
        } else {
            let name = path.to_string_lossy();
            if name.ends_with(".mir") || name.ends_with(".gleam_types.hex") {
                out.push(path);
            }
        }
    }
}

fn committed_goldens(root: &Path, suffix: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut paths = Vec::new();
    walk_goldens(root, &mut paths);
    for path in paths {
        if let Ok(relative) = path.strip_prefix(root) {
            let relative = relative.to_string_lossy();
            if let Some(stem) = relative.strip_suffix(suffix) {
                found.push(stem.to_owned());
            }
        }
    }
    found.sort();
    found
}

#[test]
fn lowers_cover_and_golden() -> Result<(), Box<dyn std::error::Error>> {
    let fixtures_root = manifest_dir().join("tests/fixtures/rev2");
    let golden_root = manifest_dir().join("tests/mir-goldens");
    let mut covered = Vec::new();
    for fixture in valid_fixtures() {
        let relative = fixture.strip_prefix(&fixtures_root)?;
        let stem = relative.with_extension("").to_string_lossy().into_owned();
        // A read/parse failure of a valid fixture is a hard error (outer `?`);
        // only a genuine lowering refusal buckets as deferred.
        match lower_fixture(&fixture)? {
            Ok(module) => {
                verify(&module)?;
                let mir = print_mir(&module);
                let sidecar = hex(&project_sidecar(&module));
                let mir_path = golden_root.join(relative).with_extension("mir");
                let sidecar_path = golden_root.join(relative).with_extension("gleam_types.hex");
                check_golden(&mir_path, &mir)?;
                check_golden(&sidecar_path, &sidecar)?;
                covered.push(stem);
            }
            Err(LowerError::Unsupported { .. } | LowerError::Planning { .. }) => {}
            Err(other) => return Err(Box::new(other)),
        }
    }
    covered.sort();

    // The covered set is pinned exactly: a fixture regressing covered → refused
    // (or a newly-covered fixture) fails here instead of being absorbed.
    let mut expected: Vec<String> = COVERED.iter().map(|s| (*s).to_owned()).collect();
    expected.sort();
    assert_eq!(
        covered, expected,
        "BC-2 covered set drifted from the pinned COVERED list"
    );

    // No orphaned goldens: both artifacts map one-for-one to covered fixtures.
    if std::env::var("AWL_BC2_BLESS").is_err() {
        let mir_goldens = committed_goldens(&golden_root, ".mir");
        let sidecar_goldens = committed_goldens(&golden_root, ".gleam_types.hex");
        assert_eq!(
            mir_goldens, covered,
            "MIR golden set does not match COVERED"
        );
        assert_eq!(
            sidecar_goldens, covered,
            "sidecar golden set does not match COVERED"
        );
    }
    Ok(())
}

#[test]
fn minimal_exercises_core_op_shapes() -> Result<(), Box<dyn std::error::Error>> {
    let path = manifest_dir().join("tests/fixtures/rev2/header-types/valid/minimal.awl");
    let module = lower_fixture(&path)??;
    verify(&module)?;
    let text = print_mir(&module);
    // §5 op shapes exercised by `host |> check |> route reported`.
    for token in [
        "call_local", // T-ACT wrapper invocation
        "call_rt aion@activity:task_queue/2",
        "call_rt aion@workflow:run/1",
        "try_bind",     // R1 flattened result.try
        "record(",      // outcome constructor + ok wrap
        "make_closure", // codec `_codec` composer
        "tail_rt aion@codec:json_codec/2",
        "field(", // record `_to_json`
        "json_obj",
    ] {
        assert!(
            text.contains(token),
            "missing op shape `{token}` in:\n{text}"
        );
    }
    Ok(())
}

/// Every pool index whose binary literal is exactly `content`, read from the
/// printed `== literals ==` table (`  [N] "content"` — the pool is
/// append-only, so one string may occupy several slots).
fn literal_indices(text: &str, content: &str) -> Vec<usize> {
    let Some(table) = text.split("== literals ==").nth(1) else {
        return Vec::new();
    };
    let quoted = format!("{content:?}");
    table
        .lines()
        .skip(1)
        .take_while(|line| line.starts_with("  ["))
        .filter_map(|line| {
            let rest = line.trim().strip_prefix('[')?;
            let (index, value) = rest.split_once("] ")?;
            (value == quoted).then(|| index.parse().ok())?
        })
        .collect()
}

/// True when `text` contains `pattern` with `{lit}` substituted by any of
/// the given literal-pool indices — content-anchored, position-flexible.
fn contains_with_lit(text: &str, pattern: &str, indices: &[usize]) -> bool {
    indices
        .iter()
        .any(|index| text.contains(&pattern.replace("{lit}", &index.to_string())))
}

/// BC-2b-5 typed-codec wire-shape pins, two-sided with
/// `tests/emitter.rs::codec_bodies_pin_the_same_wire_fragments`: the SAME
/// fragments (field-name/variant/type-name literal CONTENT included via the
/// literal table) in the direct MIR that the reference emitter renders in
/// Gleam — D4 optional omission, nullable-vs-optional-field split, composite
/// trios, and enum/union decode.failure on unknowns.
#[test]
fn typed_codec_bodies_pin_the_wire_shapes() -> Result<(), Box<dyn std::error::Error>> {
    // Record with an optional field + list field (`Note`).
    let path = manifest_dir().join("tests/fixtures/rev2/schema-doors/valid/optional_shorthand.awl");
    let module = lower_fixture(&path)??;
    verify(&module)?;
    let text = print_mir(&module);
    let body_lits = literal_indices(&text, "body");
    assert!(
        contains_with_lit(
            &text,
            "tail_rt gleam@dynamic@decode:optional_field/4(lit#{lit}, 'none', ",
            &body_lits,
        ),
        "optional field must decode via optional_field with the None default \
         (explicit null fails, D4):\n{text}"
    );
    assert!(
        text.contains("call_rt gleam@list:flatten/1")
            && text.contains("tail_rt gleam@json:object/1"),
        "optional-bearing record encode must flatten pair lists (absence \
         omitted, never null):\n{text}"
    );
    assert!(
        text.contains("== fn list_string_codec/0")
            && text.contains("tail_rt gleam@dynamic@decode:list/1"),
        "the reachable [String] composite trio must be registered:\n{text}"
    );
    assert!(
        !text.contains("decode:success/1(nil)"),
        "no decode.success(Nil) placeholder may survive:\n{text}"
    );

    // Enum + union (`Category` / `TriageMessageOutcome`).
    let path = manifest_dir().join("tests/fixtures/rev2/header-types/valid/enum.awl");
    let module = lower_fixture(&path)??;
    verify(&module)?;
    let text = print_mir(&module);
    let category_lits = literal_indices(&text, "Category");
    assert!(
        contains_with_lit(
            &text,
            "tail_rt gleam@dynamic@decode:failure/2('urgent', lit#{lit})",
            &category_lits,
        ),
        "unknown enum strings must FAIL with the first variant + type name \
         (never a silently-chosen default):\n{text}"
    );
    let outcome_lits = literal_indices(&text, "outcome");
    assert!(
        contains_with_lit(
            &text,
            "tail_rt gleam@dynamic@decode:field/3(lit#{lit}, ",
            &outcome_lits,
        ),
        "union decode must dispatch on the `outcome` field:\n{text}"
    );
    assert!(
        text.contains("== fn triage_message_outcome_decoder$outcome/1")
            && text.contains("tail_rt gleam@dynamic@decode:failure/2("),
        "unknown union outcomes must FAIL with the typed first-arm default:\n{text}"
    );
    Ok(())
}

#[test]
fn optional_rhs_is_nested_below_short_circuit_branch() -> Result<(), Box<dyn std::error::Error>> {
    let path =
        manifest_dir().join("tests/fixtures/rev2/schema-doors/valid/short_circuit_optional.awl");
    let module = lower_fixture(&path)??;
    verify(&module)?;
    let text = print_mir(&module);
    let flow = text
        .split("== fn step_decide/1")
        .nth(1)
        .ok_or("missing decide flow function")?;
    let outer_if = flow
        .find("  if is_true")
        .ok_or("missing outer guard branch")?;
    let unwrap = flow
        .find("assert_some")
        .ok_or("missing checked optional unwrap")?;
    let rhs_field = flow[unwrap..]
        .find(" = field(")
        .map(|offset| unwrap + offset)
        .ok_or("missing guarded RHS field access")?;
    assert!(
        outer_if < unwrap && unwrap < rhs_field,
        "RHS optional unwrap/field must live below the deciding branch:\n{flow}"
    );
    Ok(())
}

/// The four loop semantics pins, asserted against the reference Gleam
/// emitter's actual behavior (`emitter/loops.rs:71-125`) and independently
/// pinned by `tests/emitter.rs::loop_counter_is_language_owned`:
/// 1. post-test — the body lowers BEFORE the increment and both checks;
/// 2. the hidden count enters at 0 and increments once, after the body, so
///    the first observable counter value is 1 (completed iterations);
/// 3. `until` is tested FIRST, the ceiling SECOND, and ceiling exhaustion
///    exits `Ok` (success side) — the step's outcome clauses distinguish it;
/// 4. `until` reads the CURRENT pass's rebound value, not the entry param.
#[test]
fn loop_lowering_pins_the_reference_semantics() -> Result<(), Box<dyn std::error::Error>> {
    let path =
        manifest_dir().join("tests/fixtures/rev2/loop-outcomes/valid/loop_counting_until_max.awl");
    let module = lower_fixture(&path)??;
    verify(&module)?;
    let text = print_mir(&module);

    // Call site: count starts at 0 (pin 2).
    let call_line = text
        .lines()
        .find(|line| line.contains("call_local poll_loop_0("))
        .ok_or("missing loop call site")?;
    assert!(
        call_line.contains(", 0,"),
        "loop call site does not start the count at 0: {call_line}"
    );

    let flow = text
        .split("== fn poll_loop_0/4")
        .nth(1)
        .ok_or("missing loop function")?;
    let body_call = flow
        .find("call_local poll_activity(")
        .ok_or("missing body call")?;
    let increment = flow.find(" = increment ").ok_or("missing increment")?;
    let until_if = flow.find("if is_true").ok_or("missing until test")?;
    let bound_if = flow.find("if cmp Ge").ok_or("missing ceiling test")?;
    // Pin 1 + pin 2 + pin 3 ordering: body, then increment, then until, then
    // the ceiling.
    assert!(
        body_call < increment && increment < until_if && until_if < bound_if,
        "loop control order diverged from the reference (body -> increment -> until -> max):\n{flow}"
    );

    // Pin 3: ceiling exhaustion is success-side (`Ok`), so the outcome
    // clauses — not an error path — distinguish it.
    let bound_arm = &flow[bound_if..];
    let exhausted_exit = bound_arm
        .find("record(ok, [")
        .ok_or("ceiling arm has no Ok exit")?;
    let recurse = bound_arm
        .find("tail_local poll_loop_0(")
        .ok_or("missing self recursion")?;
    assert!(
        exhausted_exit < recurse,
        "ceiling-hit arm must exit Ok before the recurse arm:\n{bound_arm}"
    );

    // Pin 4: the until test reads a field of the try_bind result (the value
    // the body REBOUND this pass), never the entry parameter v0.
    let rebound = flow
        .lines()
        .find_map(|line| {
            let (dst, rest) = line.trim().split_once(" = try_bind ")?;
            Some((dst.to_owned(), rest.split_whitespace().next()?.to_owned()))
        })
        .ok_or("missing body try_bind")?;
    let until_read = flow[..until_if]
        .lines()
        .rev()
        .find_map(|line| {
            let (_, rest) = line.trim().split_once(" = field(")?;
            rest.split(',').next().map(str::to_owned)
        })
        .ok_or("missing until field read")?;
    assert_eq!(
        until_read, rebound.0,
        "until must see the current pass's rebound value:\n{flow}"
    );

    // The hidden count never reaches the worker call (D3): the body call's
    // arguments exclude the count parameter v1.
    let (_, args) = flow[body_call..]
        .split_once('(')
        .ok_or("malformed body call")?;
    let args = args.split(')').next().ok_or("malformed body call")?;
    assert!(
        args.split(", ").all(|arg| arg != "v1"),
        "the worker call must never carry the language-owned count: ({args})"
    );
    Ok(())
}

/// Compound `until` conditions reuse the short-circuit decision builder. The
/// continuation is cloned into each unresolved leaf, while an optional `or`
/// RHS is reachable only after the absence test fails.
#[test]
fn compound_until_clones_control_only_into_unresolved_leaves()
-> Result<(), Box<dyn std::error::Error>> {
    let path = manifest_dir()
        .join("tests/fixtures/rev2/loop-outcomes/valid/loop_compound_until_nested.awl");
    let module = lower_fixture(&path)??;
    verify(&module)?;
    let text = print_mir(&module);

    let outer = text
        .split("== fn settle_loop_0/4")
        .nth(1)
        .and_then(|tail| tail.split("== fn settle_loop_1/3").next())
        .ok_or("missing outer compound loop")?;
    let left_test = outer.find("if is_true").ok_or("missing `and` left test")?;
    let rhs_field = outer[left_test..]
        .find(" = field(")
        .map(|offset| left_test + offset)
        .ok_or("missing `and` RHS field")?;
    let rhs_test = outer[rhs_field..]
        .find("if is_true")
        .map(|offset| rhs_field + offset)
        .ok_or("missing `and` RHS test")?;
    assert!(
        left_test < rhs_field && rhs_field < rhs_test,
        "`and` RHS must be nested below its left decision:\n{outer}"
    );
    assert_eq!(
        outer.matches("if cmp Ge").count(),
        2,
        "ceiling continuation must be cloned into both unresolved `and` leaves"
    );
    assert_eq!(outer.matches("tail_local settle_loop_0(").count(), 2);

    let inner = text
        .split("== fn settle_loop_1/3")
        .nth(1)
        .ok_or("missing nested optional `or` loop")?;
    let absent_test = inner.find("cmp Eq").ok_or("missing absence test")?;
    let short_exit = inner[absent_test..]
        .find("record(ok, [")
        .map(|offset| absent_test + offset)
        .ok_or("missing short-circuit exit")?;
    let unwrap = inner
        .find("assert_some")
        .ok_or("missing narrowed RHS unwrap")?;
    assert!(
        absent_test < short_exit && short_exit < unwrap,
        "optional `or` must exit on absence before evaluating its narrowed RHS:\n{inner}"
    );
    assert_eq!(inner.matches("if cmp Ge").count(), 1);
    Ok(())
}

/// Without `counting`, the loop function returns the scalar threaded value and
/// the call site performs one `TryBind` with no tuple construction/destructure.
#[test]
fn counterless_loop_pins_the_scalar_result_path() -> Result<(), Box<dyn std::error::Error>> {
    let path =
        manifest_dir().join("tests/fixtures/rev2/loop-outcomes/valid/loop_without_counting.awl");
    let module = lower_fixture(&path)??;
    verify(&module)?;
    let text = print_mir(&module);

    let call_site = text
        .split("== fn step_refresh/2")
        .nth(1)
        .and_then(|tail| tail.split("== fn refresh_loop_0/4").next())
        .ok_or("missing counterless loop call site")?;
    assert!(call_site.contains("call_local refresh_loop_0(v2, 0, v1, v0)"));
    assert!(call_site.contains("v4 = try_bind v3"));
    assert!(!call_site.contains("field(v4, 0)"));

    let flow = text
        .split("== fn refresh_loop_0/4")
        .nth(1)
        .ok_or("missing counterless loop function")?;
    assert!(flow.contains("-> Result(refresh_cache.Cache, AwlError)"));
    assert!(!flow.contains("tuple(["));
    assert!(flow.contains("record(ok, [v8])"));
    Ok(())
}

/// Backward route re-entry resets the loop: the routed-to region re-evaluates
/// the seed and calls the loop function with count 0 — the reference
/// emitter's behavior (the spec leaves re-entry unstated; recorded in
/// AWL-BC-IR.md, exam ledger F-family).
#[test]
fn backward_route_reentry_resets_seed_and_count() -> Result<(), Box<dyn std::error::Error>> {
    let path = manifest_dir()
        .join("tests/fixtures/rev2/loop-outcomes/valid/backward_route_bounded_cycle.awl");
    let module = lower_fixture(&path)??;
    verify(&module)?;
    let text = print_mir(&module);
    let check = text
        .split("== fn step_check/")
        .nth(1)
        .ok_or("missing check step")?;
    assert!(
        check.contains("tail_local step_compose("),
        "rework must re-enter the compose region:\n{check}"
    );
    let compose = text
        .split("== fn step_compose/")
        .nth(1)
        .ok_or("missing compose step")?;
    let call_line = compose
        .lines()
        .find(|line| line.contains("call_local compose_loop_0("))
        .ok_or("missing loop call in compose")?;
    assert!(
        call_line.contains(", 0,"),
        "re-entry must reset the count to 0: {call_line}"
    );
    Ok(())
}
