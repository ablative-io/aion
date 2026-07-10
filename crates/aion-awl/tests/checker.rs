//! The rev-2 golden corpus is the checker's objective gate: every valid
//! fixture checks clean (schema imports resolved beside the fixture); every
//! CHECK-staged invalid fixture is rejected with a diagnostic containing the
//! sidecar's substring, primary span on the sidecar's line.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use aion_awl::{CheckError, check, check_in, parse};

type TestResult = Result<(), Box<dyn Error>>;

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rev2")
}

fn awl_files(dir: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut files = Vec::new();
    if !dir.exists() {
        return Ok(files);
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "awl") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn families() -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(corpus_root())? {
        let path = entry?.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

struct Expectation {
    stage: String,
    substring: String,
    line: usize,
}

fn read_expectation(path: &Path) -> Result<Expectation, Box<dyn Error>> {
    let sidecar = path.with_extension("expected");
    let content = fs::read_to_string(&sidecar)?;
    let mut lines = content.lines();
    let stage = lines.next().ok_or("sidecar missing stage line")?.to_owned();
    let substring = lines
        .next()
        .ok_or("sidecar missing substring line")?
        .to_owned();
    let line = lines
        .next()
        .ok_or("sidecar missing line-number line")?
        .trim()
        .parse::<usize>()?;
    Ok(Expectation {
        stage,
        substring,
        line,
    })
}

fn check_fixture(path: &Path) -> Result<Vec<CheckError>, Box<dyn Error>> {
    let source = fs::read_to_string(path)?;
    let document = parse(&source).map_err(|error| {
        format!(
            "{} failed to parse: {} at line {}, column {}",
            path.display(),
            error.message,
            error.span.line,
            error.span.column
        )
    })?;
    let root = path.parent().ok_or("fixture has a parent directory")?;
    Ok(check_in(&document, root))
}

#[test]
fn every_valid_fixture_checks_clean() -> TestResult {
    let mut checked = 0;
    for family in families()? {
        for path in awl_files(&family.join("valid"))? {
            let errors = check_fixture(&path)?;
            if let Some(first) = errors.first() {
                return Err(format!(
                    "{} must check clean but reported {} error(s); first: {:?} at line {}, \
                     column {}",
                    path.display(),
                    errors.len(),
                    first.message,
                    first.span.line,
                    first.span.column
                )
                .into());
            }
            checked += 1;
        }
    }
    assert!(checked >= 50, "only {checked} valid fixtures ran");
    Ok(())
}

#[test]
fn every_check_staged_invalid_fixture_is_rejected_as_expected() -> TestResult {
    let mut checked = 0;
    for family in families()? {
        for path in awl_files(&family.join("invalid"))? {
            let expectation = read_expectation(&path)?;
            if expectation.stage != "CHECK" {
                continue;
            }
            let errors = check_fixture(&path)?;
            if errors.is_empty() {
                return Err(format!("{} checked clean but must fail", path.display()).into());
            }
            let matched = errors.iter().any(|error| {
                error.message.contains(&expectation.substring)
                    && error.span.line == expectation.line
            });
            if !matched {
                return Err(format!(
                    "{}: no diagnostic contains {:?} at line {}; got {:#?}",
                    path.display(),
                    expectation.substring,
                    expectation.line,
                    errors
                        .iter()
                        .map(|error| format!("line {}: {}", error.span.line, error.message))
                        .collect::<Vec<_>>()
                )
                .into());
            }
            checked += 1;
        }
    }
    assert!(checked >= 79, "only {checked} CHECK-staged fixtures ran");
    Ok(())
}

// ---------------------------------------------------------------------
// Targeted diagnostics: exact spans and API behavior the corpus cannot pin.
// ---------------------------------------------------------------------

fn span_of(source: &str, needle: &str) -> (usize, usize, usize) {
    let Some(start) = source.find(needle) else {
        unreachable!("missing needle {needle:?}");
    };
    let prefix = &source[..start];
    let line = prefix.matches('\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    (start, line, start - line_start + 1)
}

const OPTIONAL_IMPORT_DOC: &str = "\
//! Read an imported optional field without a guard.
workflow unguarded_import
  input ticket: Ticket
  outcome triaged: type Triaged, route success

type Ticket = schema(\"ticket.schema.json\")

type Triaged { assignee: String }

worker doors
  action assign(name: String) -> Triaged

step triage
  assign(name: ticket.assignee) -> result
  result |> route triaged
";

#[test]
fn imported_optional_field_is_optional_in_the_projection_direction() -> TestResult {
    // `assignee` is absent from the import's `required`, so it projects as
    // `String?` — passing it where a plain String is declared must be
    // refused (the `?` ↔ `required` rule, JSON-to-AWL direction).
    let document = parse(OPTIONAL_IMPORT_DOC)?;
    let root = corpus_root().join("schema-doors/valid");
    let errors = check_in(&document, &root);
    let (start, line, column) = span_of(OPTIONAL_IMPORT_DOC, "ticket.assignee");
    let found = errors.iter().find(|error| {
        error.message.contains("expects String") && error.message.contains("String?")
    });
    let error = found.ok_or_else(|| format!("missing optional-misuse error; got {errors:#?}"))?;
    assert_eq!(error.span.start, start, "{error:?}");
    assert_eq!(error.span.line, line, "{error:?}");
    assert_eq!(error.span.column, column, "{error:?}");
    Ok(())
}

#[test]
fn check_without_a_root_reports_unresolvable_imports() -> TestResult {
    let document = parse(OPTIONAL_IMPORT_DOC)?;
    let errors = check(&document);
    assert!(
        errors.iter().any(|error| {
            error.message.contains("ticket.schema.json") && error.message.contains("directory")
        }),
        "missing unresolvable-import error; got {errors:#?}"
    );
    Ok(())
}

#[test]
fn unknown_binding_error_is_span_exact() -> TestResult {
    let source = "\
//! Read a binding that nothing creates.
workflow span_probe
  input name: String
  outcome done: type Out, route success

type Out { text: String }

worker w
  action shout(text: String) -> Out

step only
  shout(text: missing_binding) -> out
  out |> route done
";
    let document = parse(source)?;
    let errors = check(&document);
    let (start, line, column) = span_of(source, "missing_binding");
    let error = errors
        .iter()
        .find(|error| error.message.contains("missing_binding"))
        .ok_or_else(|| format!("missing unknown-name error; got {errors:#?}"))?;
    assert_eq!(error.span.start, start, "{error:?}");
    assert_eq!(error.span.line, line, "{error:?}");
    assert_eq!(error.span.column, column, "{error:?}");
    Ok(())
}

#[test]
fn diagnostics_arrive_sorted_by_source_position() -> TestResult {
    let source = "\
//! Two defects, reported in source order.
workflow two_defects
  input name: String
  input name: String
  outcome done: type Out, route success

type Out { text: String }

worker w
  action shout(text: String) -> Out

step only
  shout(text: ghost) -> out
  out |> route done
";
    let document = parse(source)?;
    let errors = check(&document);
    assert!(errors.len() >= 2, "expected two errors, got {errors:#?}");
    let positions: Vec<usize> = errors.iter().map(|error| error.span.start).collect();
    let mut sorted = positions.clone();
    sorted.sort_unstable();
    assert_eq!(positions, sorted);
    Ok(())
}

#[test]
fn route_targeted_earlier_step_reads_a_later_steps_binding() -> TestResult {
    // Bindings flow along the GRAPH, not file order: `hand_back` is written
    // before `produce` but is route-targeted by it, so `made` (bound in
    // `produce`) is guaranteed on every path into `hand_back`.
    let source = "\
//! Bindings flow along routes, not file order.
workflow backward_flow
  input seed: String
  outcome done: type Out, route success

type Out { text: String }

worker w
  action start(seed: String) -> Out
  action make(text: String) -> Out

step kick_off
  start(seed: seed) -> begun

  outcome always: when begun.text == \"\", route produce
  outcome fallback: otherwise, route produce

step hand_back
  made |> route done

step produce
  make(text: begun.text) -> made

  outcome always: when made.text == made.text, route hand_back
  outcome fallback: otherwise, route done(text: made.text)
";
    let document = parse(source)?;
    let errors = check(&document);
    assert_eq!(errors, Vec::new(), "must check clean");
    Ok(())
}

#[test]
fn duplicate_step_names_are_refused() -> TestResult {
    let source = "\
//! Two steps, one name.
workflow twin_steps
  input name: String
  outcome done: type Out, route success

type Out { text: String }

worker w
  action shout(text: String) -> Out

step speak
  shout(text: name) -> first_out

step speak
  first_out |> route done
";
    let document = parse(source)?;
    let errors = check(&document);
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("duplicate step `speak`")),
        "missing duplicate-step error; got {errors:#?}"
    );
    Ok(())
}
