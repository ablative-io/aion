//! Loop-specific Gleam emitter parity and defensive-backstop regressions.

use std::error::Error;

use aion_awl::{check, emit, parse};

use super::{assert_fragments_in_order, emitted_fixture, function_after};

/// Both compound `until` forms preserve source order. The optional `or` RHS
/// is emitted only in the `Some` branch, making short-circuiting observable.
#[test]
fn compound_until_is_short_circuited_in_nested_loops() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("loop-outcomes/valid/loop_compound_until_nested.awl")?;

    let inner = function_after(&generated, "fn settle_loop_1(")?;
    assert_fragments_in_order(
        inner,
        &[
            "inspect_activity(probe)",
            "let awl_count = awl_count + 1",
            "case detail {",
            "Some(detail) -> detail.approved",
            "None -> True",
            "True -> Ok(probe)",
            "False ->",
            "case awl_count >= awl_max {",
        ],
    )?;

    let outer = function_after(&generated, "fn settle_loop_0(")?;
    assert_fragments_in_order(
        outer,
        &[
            "settle_loop_1(",
            "revise_activity(probe)",
            "let awl_count = awl_count + 1",
            "case draft.ready && { draft.body == \"done\" } {",
            "True -> Ok(#(draft, awl_count))",
            "False ->",
            "case awl_count >= awl_max {",
        ],
    )?;
    Ok(())
}

/// A loop without `counting` returns and binds only its threaded value; no
/// tuple ABI or call-site destructure may leak into this legal path.
#[test]
fn counterless_loop_uses_the_scalar_result_path() -> Result<(), Box<dyn Error>> {
    let generated = emitted_fixture("loop-outcomes/valid/loop_without_counting.awl")?;
    assert!(
        generated.contains("use cache <- result.try(refresh_loop_0("),
        "counterless call site must bind the scalar result: {generated}"
    );
    assert!(
        generated.contains(", 0, max_attempts, key))"),
        "counterless loops still initialize the hidden ceiling counter: {generated}"
    );
    let flow = function_after(&generated, "fn refresh_loop_0(")?;
    assert!(flow.contains("True -> Ok(cache)"));
    assert!(!flow.contains("Ok(#(cache, awl_count))"));
    Ok(())
}

/// The checker introduces a named counter only after its loop, and public
/// `emit` refuses documents that do not check cleanly at the door — so the
/// illegal in-`until` counter reference surfaces as the checker's own
/// diagnostic rather than a generated unbound identifier.
#[test]
fn unchecked_counter_is_not_in_until_scope() -> Result<(), Box<dyn Error>> {
    let source = "\
//! Unchecked loop counter scope regression.
workflow counter_scope
  input max_rounds: Int
  outcome done: type Count, route success

type State { value: String }
type Count { value: Int }

worker states
  action tick(prior: State) -> State

step run
  loop state = State(value: \"\") counting rounds
    tick(prior: state) -> state
    until rounds > 2
    max max_rounds

  outcome complete: otherwise,
    route done(value: rounds)
";
    let document = parse(source)?;
    let error = emit(&document)
        .err()
        .ok_or("unchecked emit must reject a counter referenced inside its own loop")?;
    assert!(
        error.to_string().contains("does not check cleanly")
            && error
                .to_string()
                .contains("`rounds` is bound on some path but not guaranteed"),
        "unexpected counter-scope error: {error}"
    );
    Ok(())
}

/// A counter cannot share the threaded binding's name: the two values have
/// distinct types and would require an illegal duplicate Gleam tuple binder.
#[test]
fn counting_name_collision_is_checker_refused() -> Result<(), Box<dyn Error>> {
    let source = "\
//! Counter collision regression.
workflow counter_collision
  input max_rounds: Int
  outcome done: type Count, route success

type State { value: String }
type Count { value: Int }

worker states
  action tick(prior: State) -> State

step run
  loop state = State(value: \"\") counting state
    tick(prior: state) -> state
    until true
    max max_rounds

  outcome complete: otherwise,
    route done(value: state)
";
    let document = parse(source)?;
    let errors = check(&document);
    let collision = errors
        .iter()
        .find(|error| error.message == "`counting` name must differ from the loop binding")
        .ok_or_else(|| format!("missing collision diagnostic in {errors:?}"))?;
    assert_eq!(&source[collision.span.start..collision.span.end], "state");
    Ok(())
}

/// A `route` inside a `loop` body is a language-level check error since the
/// 2026-07-11 ruling (loops exit via `until`/`max`; routing belongs to the
/// step's outcome clauses) — the user-facing refusal lives in `check`. The
/// emitter keeps this spanned refusal as the defensive backstop for `emit`
/// called on an UNCHECKED document, where the route could never reach tail
/// position in the generated loop function (originally a 2026-07-11 emitter
/// panel blocking finding).
#[test]
fn route_inside_a_loop_body_is_refused() -> Result<(), Box<dyn Error>> {
    let source = "\
//! Loop route stress.
workflow loop_route
  input target: String
  outcome done: type Report, route success
  outcome bailed: type Report, route failure

type Round  { summary: String, gates_green: Bool }
type Report { summary: String }

worker builder
  action work(prior: Round) -> Round

step build
  loop round = Round(summary: \"\", gates_green: false) counting cycles
    work(prior: round) -> round
    route bailed(summary: round.summary)
    until round.gates_green
    max 3

  outcome green: when round.gates_green, route done(summary: round.summary)
  outcome spent: otherwise, route done(summary: \"spent\")
";
    let document = parse(source)?;
    let error = emit(&document).err().ok_or("emit must refuse")?;
    assert!(
        error.message.contains("`route` inside a `loop` body"),
        "unexpected message: {}",
        error.message
    );
    let route_line = source
        .lines()
        .position(|line| line.contains("route bailed"))
        .ok_or("missing route line")?
        + 1;
    assert_eq!(error.span.line, route_line, "span must anchor the route");
    Ok(())
}

/// A loop `max` referencing the loop-threaded value renders at the call
/// site, where no loop-local exists (2026-07-11 emitter panel, blocking
/// finding). The checker refuses the shape in the pre-loop scope, and public
/// `emit` refuses unchecked documents at the door, so that diagnostic is
/// what surfaces; the emitter's own refusal remains behind the gate.
#[test]
fn loop_max_must_be_loop_invariant() -> Result<(), Box<dyn Error>> {
    let source = "\
//! Loop ceiling stress.
workflow loop_ceiling
  input job: String
  outcome done: type Report, route success

type Round  { summary: String, gates_green: Bool, budget: Int }
type Report { summary: String }

worker builder
  action work(prior: Round) -> Round

step build
  loop round = Round(summary: \"\", gates_green: false, budget: 3) counting cycles
    work(prior: round) -> round
    until round.gates_green
    max round.budget

  outcome green: when round.gates_green, route done(summary: round.summary)
  outcome spent: otherwise, route done(summary: \"spent\")
";
    let document = parse(source)?;
    let error = emit(&document).err().ok_or("emit must refuse")?;
    assert!(
        error.message.contains("does not check cleanly")
            && error.message.contains("`max` is the loop's ceiling"),
        "unexpected message: {}",
        error.message
    );
    let max_line = source
        .lines()
        .position(|line| line.contains("max round.budget"))
        .ok_or("missing max line")?
        + 1;
    assert_eq!(error.span.line, max_line, "span must anchor the ceiling");
    Ok(())
}
