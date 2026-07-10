//! The three ratified language rulings (Tom, 2026-07-11), pinned beyond the
//! corpus fixtures' substring/line contract:
//!
//! R1. Loop exhaustion must be explicitly named — a loop-carrying step with
//!     zero outcome clauses is a check error anchored at the loop.
//! R2. `?` is illegal in list-element position — `[T?]` is refused in every
//!     type position; `[T]?` (the whole list absent) stays legal.
//! R3. `route` is illegal inside a `loop` body — statement and pipe-chain
//!     terminator alike; the refusal is the checker's (the emitter keeps a
//!     defensive backstop for unchecked documents).

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
// R1 — loop exhaustion must be explicitly named
// ---------------------------------------------------------------------

/// A loop inside a collection-fork branch still puts the exhaustion duty on
/// the owning step: the scan looks through fork bodies.
#[test]
fn loop_inside_a_fork_branch_still_requires_outcome_clauses() -> TestResult {
    let source = "\
//! Each branch polls to completion; the step never names exhaustion.
workflow fan_poll
  input job_ids: [String]
  outcome done: type Tally, route success

type Poll  { state: String, done: Bool }
type Tally { polled: Int }

worker poller
  action poll(job_id: String, prior: Poll) -> Poll

step poll_all
  fork job_id in job_ids
    loop status = Poll(state: \"pending\", done: false)
      poll(job_id: job_id, prior: status) -> status
      until status.done
      max 5
  join -> results

step report
  results |> count -> polled_count
  route done(polled: polled_count)
";
    let errors = check_source(source)?;
    let line = line_of(source, "loop status")?;
    find_error(&errors, "exhaust", line)?;
    Ok(())
}

/// A loop inside a substep answers to the substep's own outcome clauses,
/// not the parent's: the parent-body scan skips substeps.
#[test]
fn loop_inside_a_substep_answers_to_the_substep() -> TestResult {
    let source = "\
//! The substep loops with no outcome clauses; the error names the substep.
workflow staged_poll
  input job_id: String
  input max_polls: Int
  outcome done: type Poll, route success

type Poll { state: String, done: Bool }

worker poller
  action poll(job_id: String, prior: Poll) -> Poll

step run
  step inner
    loop status = Poll(state: \"pending\", done: false) counting polls
      poll(job_id: job_id, prior: status) -> status
      until status.done
      max max_polls

  outcome always: when status.done,
    route done(state: status.state, done: status.done)
  outcome fallback: otherwise,
    route done(state: status.state, done: status.done)
";
    let errors = check_source(source)?;
    let line = line_of(source, "loop status")?;
    let error = find_error(&errors, "exhaust", line)?;
    assert!(
        error.message.contains("`inner`"),
        "the diagnostic must name the substep: {}",
        error.message
    );
    Ok(())
}

// ---------------------------------------------------------------------
// R2 — `?` is illegal in list-element position
// ---------------------------------------------------------------------

/// `[T?]` is refused in input position with a span on the element type,
/// and the diagnostic suggests the legal spellings.
#[test]
fn list_element_optional_is_refused_in_input_position() -> TestResult {
    let source = "\
//! An input tries to smuggle optional elements into a list.
workflow gap_list
  input names: [String?]
  outcome done: type Out, route success

type Out { text: String }

worker w
  action first_name(names: [String]) -> Out

step only
  first_name(names: names) -> out
  out |> route done
";
    let errors = check_source(source)?;
    let line = line_of(source, "[String?]")?;
    let error = find_error(&errors, "element", line)?;
    assert!(
        error.message.contains("[String]") && error.message.contains("[String]?"),
        "the diagnostic must suggest [String] and [String]?: {}",
        error.message
    );
    let column = source
        .lines()
        .nth(line - 1)
        .and_then(|text| text.find("String?"))
        .ok_or("missing element type")?
        + 1;
    assert_eq!(error.span.column, column, "{error:?}");
    Ok(())
}

/// The rule covers outcome declarations and child signatures too.
#[test]
fn list_element_optional_is_refused_in_outcome_and_child_positions() -> TestResult {
    let source = "\
//! Outcome and child signatures try optional elements.
workflow spread
  input seed: String
  outcome done: type [Int?], route success

child expand(seeds: [String?]) -> [Int]

step only
  expand(seeds: [seed]) -> total
  total |> route done
";
    let errors = check_source(source)?;
    find_error(&errors, "element", line_of(source, "[Int?]")?)?;
    find_error(&errors, "element", line_of(source, "[String?]")?)?;
    Ok(())
}

/// Nested element optionality is refused wherever it appears.
#[test]
fn nested_list_element_optional_is_refused() -> TestResult {
    let source = "\
//! The inner list of a list-of-lists tries optional elements.
workflow matrix
  input rows: [[String?]]
  outcome done: type Out, route success

type Out { text: String }

worker w
  action flatten(rows: [[String]]) -> Out

step only
  flatten(rows: rows) -> out
  out |> route done
";
    let errors = check_source(source)?;
    find_error(&errors, "element", line_of(source, "[[String?]]")?)?;
    Ok(())
}

/// `[T]?` — the list itself may be absent — remains legal everywhere.
#[test]
fn optional_list_stays_legal() -> TestResult {
    let source = "\
//! The whole list may be absent; its elements may not.
workflow maybe_list
  input tags: [String]?
  outcome done: type Out, route success

type Out { tagged: Bool }

worker w
  action classify(tags: [String]) -> Out

step only
  classify(tags: [\"seed\"]) -> seeded

  outcome present_tags: when tags is present, route apply
  outcome bare: otherwise, route apply

step apply
  classify(tags: [\"a\"]) -> out
  out |> route done
";
    let errors = check_source(source)?;
    assert_eq!(errors, Vec::new(), "must check clean");
    Ok(())
}

// ---------------------------------------------------------------------
// R3 — `route` is illegal inside a `loop` body
// ---------------------------------------------------------------------

/// The refusal is the checker's and sees through nested blocks: a route
/// inside a fork inside a loop is refused with the span on the route.
#[test]
fn route_nested_in_a_fork_inside_a_loop_is_refused_by_check() -> TestResult {
    let source = "\
//! A fork branch inside the loop tries to route away mid-iteration.
workflow deep_route
  input targets: [String]
  input max_rounds: Int

  outcome done:   type Round, route success
  outcome bailed: type Round, route failure

type Round { summary: String, gates_green: Bool }

worker builder
  action build_round(target: String, prior: Round) -> Round
  action merge(rounds: [Round], prior: Round) -> Round

step build
  loop round = Round(summary: \"\", gates_green: false)
    fork target in targets
      build_round(target: target, prior: round) -> attempt
      route bailed(summary: attempt.summary, gates_green: false)
    join -> rounds
    merge(rounds: rounds, prior: round) -> round
    until round.gates_green
    max max_rounds

  outcome green: when round.gates_green,
    route done(summary: round.summary, gates_green: round.gates_green)
  outcome spent: otherwise,
    route bailed(summary: \"spent\", gates_green: false)
";
    let errors = check_source(source)?;
    let line = line_of(source, "route bailed(summary: attempt.summary")?;
    let error = find_error(&errors, "`loop` body", line)?;
    assert!(
        error.message.contains("until") && error.message.contains("outcome clauses"),
        "the diagnostic must explain the loop exit and where routing lives: {}",
        error.message
    );
    Ok(())
}

/// The same shapes outside a loop stay legal: a body `route` statement and
/// a pipe-chain `route` terminator are untouched by the rule.
#[test]
fn routes_outside_loop_bodies_stay_legal() -> TestResult {
    let source = "\
//! Routing from step bodies is untouched by the loop rule.
workflow plain_routes
  input name: String
  outcome done: type Out, route success

type Out { text: String }

worker w
  action shout(text: String) -> Out

step speak
  shout(text: name) -> out

step finish
  out |> route done
";
    let errors = check_source(source)?;
    assert_eq!(errors, Vec::new(), "must check clean");
    Ok(())
}
