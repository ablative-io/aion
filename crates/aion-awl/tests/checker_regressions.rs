//! Checker fix-round regressions (2026-07-11 adversarial panel): each test
//! pins one confirmed finding so the defect class stays unwritable.
//!
//! 1. Named-branch fork branches are scope-isolated (parallel semantics).
//! 2. Every non-terminal step needs a successor, not just the final step.
//! 3. A pipe chain may not terminate in `route <step>` (silent value loss).
//! 5. A backward route plus a forward `after` edge is a cycle needing a
//!    bound.
//! 6. A step may not share its name with a workflow outcome (one route
//!    namespace).
//! 7. A binding inside a collection-fork branch is not a loop rebind.
//!
//! The schema-anchor and type-system findings (4, 8) and the advisory
//! hardening live in `checker_hardening.rs`.

use std::error::Error;

use aion_awl::{CheckError, check, parse};

type TestResult = Result<(), Box<dyn Error>>;

fn check_source(source: &str) -> Result<Vec<CheckError>, Box<dyn Error>> {
    let document = parse(source).map_err(|error| {
        format!(
            "failed to parse: {} at line {}, column {}",
            error.message, error.span.line, error.span.column
        )
    })?;
    Ok(check(&document))
}

fn line_of(source: &str, needle: &str) -> Result<usize, Box<dyn Error>> {
    let start = source
        .find(needle)
        .ok_or_else(|| format!("missing needle {needle:?}"))?;
    Ok(source[..start].matches('\n').count() + 1)
}

fn find_error<'e>(
    errors: &'e [CheckError],
    substring: &str,
    line: usize,
) -> Result<&'e CheckError, Box<dyn Error>> {
    errors
        .iter()
        .find(|error| error.message.contains(substring) && error.span.line == line)
        .ok_or_else(|| {
            format!(
                "no diagnostic contains {substring:?} at line {line}; got {:#?}",
                errors
                    .iter()
                    .map(|error| format!("line {}: {}", error.span.line, error.message))
                    .collect::<Vec<_>>()
            )
            .into()
        })
}

// ---------------------------------------------------------------------
// 1. Named-branch fork scope isolation
// ---------------------------------------------------------------------

#[test]
fn named_fork_branch_cannot_read_a_sibling_bindings() -> TestResult {
    let source = "\
//! Parallel branches cannot read each other.
workflow fork_isolation
  input id: String
  outcome done: type Out, route success

type Out     { text: String }
type Profile { name: String }
type History { events: [String] }

worker w
  action fetch_profile(id: String) -> Profile
  action fetch_history(hint: String) -> History
  action summarize(name: String) -> Out

step gather
  fork
    fetch_profile(id: id) -> profile
    fetch_history(hint: profile.name) -> history
  join

step finish
  summarize(name: profile.name) -> out
  out |> route done
";
    let errors = check_source(source)?;
    // The read inside the sibling branch is refused …
    let line = line_of(source, "fetch_history(hint: profile.name)")?;
    find_error(&errors, "profile", line)?;
    // … and it is the only defect: after `join` the branch bindings merge,
    // so `finish` reading `profile` is legal.
    assert_eq!(
        errors.len(),
        1,
        "expected exactly one error; got {errors:#?}"
    );
    Ok(())
}

#[test]
fn named_fork_join_binding_is_refused() -> TestResult {
    let source = "\
//! The named-branch form joins without a binding.
workflow named_join_bind
  input id: String
  outcome done: type Out, route success

type Out     { text: String }
type Profile { name: String }
type History { events: [String] }

worker w
  action fetch_profile(id: String) -> Profile
  action fetch_history(hint: String) -> History
  action summarize(name: String) -> Out

step gather
  fork
    fetch_profile(id: id) -> profile
    fetch_history(hint: id) -> history
  join -> what

step finish
  summarize(name: profile.name) -> out
  out |> route done
";
    let errors = check_source(source)?;
    let line = line_of(source, "join -> what")?;
    find_error(&errors, "named-branch fork joins without a binding", line)?;
    Ok(())
}

// ---------------------------------------------------------------------
// 2. Every non-terminal step has a successor
// ---------------------------------------------------------------------

#[test]
fn dangling_middle_step_is_refused() -> TestResult {
    let source = "\
//! A stranded middle step: nothing consumes its completion.
workflow stranded_middle
  input seed: String
  outcome done: type Out, route success

type Out { text: String }

worker w
  action make(text: String) -> Out

step start
  make(text: seed) -> begun

step stranded
  make(text: begun.text) -> lost

step finish after start
  make(text: begun.text) -> out
  out |> route done
";
    let errors = check_source(source)?;
    let line = line_of(source, "step stranded")?;
    find_error(&errors, "successor", line)?;
    Ok(())
}

#[test]
fn falling_step_consumed_by_an_after_dependent_is_legal() -> TestResult {
    let source = "\
//! `after` on a later step consumes the fall-through completion.
workflow after_consumes
  input seed: String
  outcome done: type Out, route success

type Out { text: String }

worker w
  action make(text: String) -> Out

step start
  make(text: seed) -> begun

step warm
  make(text: begun.text) -> warmth

step finish after warm
  make(text: warmth.text) -> out
  out |> route done
";
    let errors = check_source(source)?;
    assert_eq!(errors, Vec::new(), "must check clean");
    Ok(())
}

// ---------------------------------------------------------------------
// 3. Piped route must target a workflow outcome
// ---------------------------------------------------------------------

#[test]
fn piped_route_to_a_step_is_refused() -> TestResult {
    let source = "\
//! The piped value has nowhere to go on a step target.
workflow piped_to_step
  input seed: String
  outcome done: type Out, route success

type Out { text: String }

worker w
  action make(text: String) -> Out

step first
  make(text: seed) -> out
  out |> route second

step second
  make(text: out.text) -> fin
  fin |> route done
";
    let errors = check_source(source)?;
    let line = line_of(source, "out |> route second")?;
    let error = find_error(&errors, "piped", line)?;
    assert!(
        error.message.contains("step"),
        "the refusal must say the target is a step: {error:?}"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// 5. Backward route + forward `after` edge is an unbounded cycle
// ---------------------------------------------------------------------

#[test]
fn backward_route_re_armed_by_an_after_edge_needs_a_bound() -> TestResult {
    let source = "\
//! Rework cycle formed by one route and one `after` edge, no bound.
workflow rework_cycle
  input topic: String
  outcome done: type Out, route success

type Out     { text: String }
type Draft   { body: String }
type Verdict { approved: Bool }

worker w
  action polish(topic: String) -> Draft
  action review(draft: Draft) -> Verdict
  action finalize(draft: Draft) -> Out

step compose
  polish(topic: topic) -> draft

step check after compose
  review(draft: draft) -> verdict

  outcome approved: when verdict.approved, route publish
  outcome rework: otherwise, route compose

step publish
  finalize(draft: draft) -> out
  out |> route done
";
    let errors = check_source(source)?;
    let line = line_of(source, "otherwise, route compose")?;
    let error = find_error(&errors, "cycle", line)?;
    assert!(
        error.message.contains("bound"),
        "the diagnostic demands a bound: {error:?}"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// 6. Steps and workflow outcomes share one route-target namespace
// ---------------------------------------------------------------------

#[test]
fn step_sharing_a_workflow_outcome_name_is_refused() -> TestResult {
    let source = "\
//! `route done` must never be ambiguous.
workflow collide
  input seed: String
  outcome done: type Out, route success
  outcome fin:  type Out, route success

type Out { text: String }

worker w
  action make(text: String) -> Out

step done
  make(text: seed) -> out
  out |> route fin
";
    let errors = check_source(source)?;
    let line = line_of(source, "step done")?;
    find_error(&errors, "namespace", line)?;
    Ok(())
}

// ---------------------------------------------------------------------
// 7. Collection-fork branch bindings are not loop rebinds
// ---------------------------------------------------------------------

#[test]
fn loop_rebind_inside_a_collection_fork_branch_does_not_count() -> TestResult {
    let source = "\
//! The branch binding never escapes; the loop threads nothing.
workflow fork_rebind
  input topics: [String]
  input max_rounds: Int
  outcome done: type Out, route success

type Out   { text: String }
type Draft { body: String, ready: Bool }

worker w
  action make(topic: String) -> Draft
  action finish(draft: Draft) -> Out

step build
  loop draft = Draft(body: \"\", ready: false) counting rounds
    fork topic in topics
      make(topic: topic) -> draft
    join
    until draft.ready
    max max_rounds

  outcome ready: when draft.ready, route wrap
  outcome spent: otherwise, route wrap

step wrap
  finish(draft: draft) -> out
  out |> route done
";
    let errors = check_source(source)?;
    let line = line_of(source, "loop draft")?;
    find_error(&errors, "never rebinds `draft`", line)?;
    Ok(())
}

// ---------------------------------------------------------------------
// 8. Loop `max` is typed in the pre-loop scope (2026-07-11 emitter panel:
//    the ceiling renders at the loop call site, where loop-locals do not
//    exist — a body-scoped `max` checked clean but could not compile)
// ---------------------------------------------------------------------

#[test]
fn loop_max_may_not_read_loop_locals() -> TestResult {
    let source = "\
//! The ceiling is fixed before the first pass.
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
    let errors = check_source(source)?;
    let line = line_of(source, "max round.budget")?;
    find_error(&errors, "loop-local", line)?;
    Ok(())
}

#[test]
fn loop_max_may_not_read_the_counter() -> TestResult {
    let source = "\
//! The counter is no ceiling either.
workflow loop_counter_ceiling
  input job: String
  outcome done: type Report, route success

type Round  { summary: String, gates_green: Bool }
type Report { summary: String }

worker builder
  action work(prior: Round) -> Round

step build
  loop round = Round(summary: \"\", gates_green: false) counting cycles
    work(prior: round) -> round
    until round.gates_green
    max cycles

  outcome green: when round.gates_green, route done(summary: round.summary)
  outcome spent: otherwise, route done(summary: \"spent\")
";
    let errors = check_source(source)?;
    let line = line_of(source, "max cycles")?;
    find_error(&errors, "loop-local", line)?;
    Ok(())
}

#[test]
fn loop_max_over_inputs_stays_clean() -> TestResult {
    let source = "\
//! An input-derived ceiling is the sanctioned shape.
workflow loop_input_ceiling
  input job: String
  input budget: Int
  outcome done: type Report, route success

type Round  { summary: String, gates_green: Bool }
type Report { summary: String }

worker builder
  action work(prior: Round) -> Round

step build
  loop round = Round(summary: \"\", gates_green: false) counting cycles
    work(prior: round) -> round
    until round.gates_green
    max budget

  outcome green: when round.gates_green, route done(summary: round.summary)
  outcome spent: otherwise, route done(summary: \"spent\")
";
    let errors = check_source(source)?;
    assert!(errors.is_empty(), "expected clean, got {errors:#?}");
    Ok(())
}
