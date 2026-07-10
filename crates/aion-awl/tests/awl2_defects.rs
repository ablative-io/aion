//! Span-discipline regression tests, ported to the rev-2 grammar from the
//! AWL-0 adversarial-review defect suite:
//!
//! 1. Errors deep in a document report true lines and byte offsets, never
//!    fragment-relative positions.
//! 2. AST nodes buried in `parse()`'s output carry document-true spans
//!    (proved against the committed flagship fixture, not synthetic text).
//! 3. Own-line comments survive parse ↔ print at every nesting level, in
//!    order, idempotently.
//! 4. Dead AWL-0/1 keywords produce targeted fix-it diagnostics naming the
//!    rev-2 replacement, anchored on the offending word.
//! 5. Columns are character-based: multibyte content earlier on a line
//!    does not skew a diagnostic's column.

use std::error::Error;
use std::fmt::Write as _;

use aion_awl::{ForkHeader, Spanned, Statement, parse, print};

type TestResult = Result<(), Box<dyn Error>>;

// ---------------------------------------------------------------------
// 1. Source-correct spans for deep-document errors
// ---------------------------------------------------------------------

/// Build a rev-2 document with a syntactically broken `when` guard on a
/// caller-chosen step, padded above with valid filler steps (three lines
/// each) plus extra narration lines (one line each, for fine tuning) so
/// the error sits deep in a multi-line document rather than near line 1.
fn doc_with_broken_guard(filler_steps: usize, narration_pad: usize) -> String {
    let mut src = String::new();
    for index in 0..narration_pad {
        let _ = writeln!(src, "//! Narration padding line {index}.");
    }
    src.push_str(
        "//! Padded fixture for span discipline.\n\
         workflow padded\n\
         \x20 input a: String\n\
         \x20 outcome done: type Out, route success\n\
         \n\
         type Out { text: String }\n\
         \n\
         worker w\n\
         \x20 action noop(a: String) -> Out\n\
         \n",
    );
    for index in 0..filler_steps {
        let _ = write!(src, "step filler{index}\n  noop(a: a) -> r{index}\n\n");
    }
    src.push_str(
        "step bad\n\
         \x20 noop(a: a) -> out\n\
         \n\
         \x20 outcome broken: when 1 +\n\
         \x20 outcome fallback: otherwise, route done\n",
    );
    src
}

#[test]
fn guard_error_deep_in_document_reports_true_line_and_offset() -> TestResult {
    let source = doc_with_broken_guard(12, 0);
    let Err(error) = parse(&source) else {
        return Err("`when 1 +` should be rejected".into());
    };

    let anchor = source.find("when 1 +").ok_or("fragment present")?;
    let expected_line = source[..anchor].matches('\n').count() + 1;
    assert!(
        expected_line > 30,
        "fixture didn't push the error deep enough: line {expected_line}"
    );
    assert_eq!(
        error.span.line, expected_line,
        "error must report the document line, not a fragment-relative one: {error:?}"
    );
    let line_start = source[..anchor].rfind('\n').map_or(0, |at| at + 1);
    let line_end = source[anchor..]
        .find('\n')
        .map_or(source.len(), |at| anchor + at);
    assert!(
        error.span.start >= line_start && error.span.start <= line_end,
        "error byte offset {} escapes the broken line {line_start}..{line_end}",
        error.span.start
    );
    Ok(())
}

#[test]
fn guard_error_at_exactly_line_forty_is_source_correct() -> TestResult {
    let mut knobs = (0, 0);
    let source = loop {
        let candidate = doc_with_broken_guard(knobs.0, knobs.1);
        let broken_line = candidate
            .lines()
            .position(|line| line.trim() == "outcome broken: when 1 +")
            .ok_or("broken line present")?
            + 1;
        if broken_line == 40 {
            break candidate;
        }
        if knobs.1 < 2 {
            knobs.1 += 1;
        } else {
            knobs = (knobs.0 + 1, 0);
        }
        assert!(knobs.0 < 200, "could not land the broken line on line 40");
    };

    let Err(error) = parse(&source) else {
        return Err("`when 1 +` should be rejected".into());
    };
    assert_eq!(error.span.line, 40, "{error:?}");
    Ok(())
}

// ---------------------------------------------------------------------
// 2. Document-true spans on AST nodes, proved against the flagship
// ---------------------------------------------------------------------

#[test]
fn fork_collection_span_matches_flagship_true_position() -> TestResult {
    let source = include_str!("fixtures/rev2/flagship/valid/dev_brief.awl");
    let document = parse(source)?;
    let review = document
        .steps
        .iter()
        .find(|step| step.name == "review")
        .ok_or("review step present")?;
    let Some(Statement::Fork(fork)) = review.body.first() else {
        return Err("review opens with a fork".into());
    };
    let ForkHeader::Collection { collection, .. } = &fork.header else {
        return Err("flagship fork is the collection form".into());
    };

    let anchor = source
        .find("fork lens in config.lenses")
        .ok_or("fork line present")?;
    let expected_start = anchor + "fork lens in ".len();
    let expected_line = source[..anchor].matches('\n').count() + 1;
    assert_eq!(collection.span().start, expected_start);
    assert_eq!(collection.span().line, expected_line);
    Ok(())
}

#[test]
fn action_config_duration_span_matches_flagship_true_position() -> TestResult {
    let source = include_str!("fixtures/rev2/flagship/valid/dev_brief.awl");
    let document = parse(source)?;
    let worker = document.workers.first().ok_or("worker present")?;
    let provision = worker
        .actions
        .iter()
        .find(|action| action.name == "provision")
        .ok_or("provision action present")?;
    let config = provision.config.as_ref().ok_or("config line present")?;
    let timeout = config.timeout.as_ref().ok_or("timeout present")?;

    // "timeout 5m" appears once, on provision's config line.
    let anchor = source.find("timeout 5m").ok_or("timeout clause present")?;
    let expected_start = anchor + "timeout ".len();
    let expected_line = source[..anchor].matches('\n').count() + 1;
    assert_eq!(timeout.span.start, expected_start);
    assert_eq!(timeout.span.line, expected_line);
    Ok(())
}

// ---------------------------------------------------------------------
// 3. Own-line comment trivia at every nesting level
// ---------------------------------------------------------------------

const COMMENT_TORTURE_DOC: &str = concat!(
    "//! Comment placement torture test.\n",
    "workflow torture\n",
    "  // leading the first input\n",
    "  input a: String\n",
    "  // leading an outcome decl\n",
    "  outcome done: type Out, route success\n",
    "\n",
    "// leading a type\n",
    "type Out {\n",
    "  // leading a field\n",
    "  text: String,\n",
    "}\n",
    "\n",
    "// leading a worker\n",
    "worker w\n",
    "  // leading an action\n",
    "  action make(a: String) -> Out\n",
    "    // leading a config line\n",
    "    node shell, timeout 5s\n",
    "\n",
    "// leading a step\n",
    "step one\n",
    "  // leading a call\n",
    "  make(a: a) -> out\n",
    "  // leading a loop\n",
    "  loop x = Out(text: \"\") counting n\n",
    "    make(a: a) -> x\n",
    "    // leading until\n",
    "    until true\n",
    "    // leading max\n",
    "    max 3\n",
    "  // leading on failure\n",
    "  on failure\n",
    "    // leading a handler call\n",
    "    make(a: a)\n",
    "    // leading a handler route\n",
    "    route done(text: \"failed\")\n",
    "  // leading an outcome clause\n",
    "  outcome ok: when true, route done\n",
);

#[test]
fn own_line_comments_survive_at_every_nesting_level() -> TestResult {
    let printed = print(&parse(COMMENT_TORTURE_DOC)?);

    let expectations = [
        "  // leading the first input",
        "  // leading an outcome decl",
        "// leading a type",
        "  // leading a field",
        "// leading a worker",
        "  // leading an action",
        "    // leading a config line",
        "// leading a step",
        "  // leading a call",
        "  // leading a loop",
        "    // leading until",
        "    // leading max",
        "  // leading on failure",
        "    // leading a handler call",
        "    // leading a handler route",
        "  // leading an outcome clause",
    ];
    let mut previous = 0;
    for needle in expectations {
        let found = printed
            .find(needle)
            .ok_or_else(|| format!("missing or misindented comment {needle:?} in:\n{printed}"))?;
        assert!(
            found >= previous,
            "comment {needle:?} printed out of order:\n{printed}"
        );
        previous = found;
    }
    Ok(())
}

#[test]
fn comment_torture_document_round_trips_byte_identically() -> TestResult {
    let first = print(&parse(COMMENT_TORTURE_DOC)?);
    assert_eq!(
        first, COMMENT_TORTURE_DOC,
        "torture document is authored canonically; print must reproduce it"
    );
    let second = print(&parse(&first)?);
    assert_eq!(first, second);
    Ok(())
}

// ---------------------------------------------------------------------
// 4. Dead-keyword fix-its name the rev-2 replacement
// ---------------------------------------------------------------------

fn doc_with_body_line(line: &str) -> String {
    format!(
        "//! Dead keyword probe.\n\
         workflow probe\n\
         \x20 input a: String\n\
         \x20 outcome done: type Out, route success\n\
         \n\
         type Out {{ text: String }}\n\
         \n\
         worker w\n\
         \x20 action make(a: String) -> Out\n\
         \n\
         step one\n\
         \x20 {line}\n"
    )
}

fn doc_with_header_line(line: &str) -> String {
    format!(
        "//! Dead keyword probe.\n\
         workflow probe\n\
         \x20 input a: String\n\
         \x20 {line}\n\
         \x20 outcome done: type Out, route success\n\
         \n\
         type Out {{ text: String }}\n\
         \n\
         worker w\n\
         \x20 action make(a: String) -> Out\n\
         \n\
         step one\n\
         \x20 a |> make |> route done\n"
    )
}

fn assert_fix_it(
    source: &str,
    probe_line: &str,
    dead_word: &str,
    replacement_hint: &str,
) -> TestResult {
    let Err(error) = parse(source) else {
        return Err(format!("`{dead_word}` should be rejected").into());
    };
    assert!(
        error.message.contains(dead_word),
        "diagnostic must name the dead keyword `{dead_word}`: {:?}",
        error.message
    );
    assert!(
        error.message.contains(replacement_hint),
        "diagnostic for `{dead_word}` must point at `{replacement_hint}`: {:?}",
        error.message
    );
    let anchor = source
        .find(&format!("\n  {probe_line}"))
        .ok_or("probe line present")?
        + 1;
    let expected_line = source[..anchor].matches('\n').count() + 1;
    assert_eq!(error.span.line, expected_line, "{:?}", error.message);
    Ok(())
}

#[test]
fn dead_statement_keywords_get_targeted_fix_its() -> TestResult {
    let cases = [
        ("do make(a: a)", "do", "->"),
        ("as out", "as", "->"),
        ("each q in questions", "each", "fork"),
        ("repeat up to 3", "repeat", "loop"),
        ("match category", "match", "outcome"),
        ("parallel", "parallel", "fork"),
        ("race", "race", "timeout"),
        ("fail", "fail", "route <workflow outcome>"),
    ];
    for (line, word, hint) in cases {
        assert_fix_it(&doc_with_body_line(line), line, word, hint)?;
    }
    Ok(())
}

#[test]
fn dead_header_keywords_get_targeted_fix_its() -> TestResult {
    let cases = [
        ("output String", "output", "success"),
        ("error Failed", "error", "failure"),
        ("queue \"q1\"", "queue", "worker"),
    ];
    for (line, word, hint) in cases {
        assert_fix_it(&doc_with_header_line(line), line, word, hint)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------
// 5. Character-correct columns after multibyte content
// ---------------------------------------------------------------------

#[test]
fn error_column_after_multibyte_prose_is_character_correct() -> TestResult {
    // The string literal carries multibyte characters (é, —, ï); the
    // defective `->` with no binding name sits after it on the same line.
    let line = "  make(a: \"café — naïve\") ->";
    let source = doc_with_body_line(line.trim_start());
    let Err(error) = parse(&source) else {
        return Err("dangling `->` should be rejected".into());
    };

    let anchor = source.find("\") ->").ok_or("arrow present")? + "\") ".len();
    let expected_line = source[..anchor].matches('\n').count() + 1;
    assert_eq!(error.span.line, expected_line);
    assert_eq!(error.span.start, anchor, "byte offset must be byte-true");

    let line_start = source[..anchor].rfind('\n').map_or(0, |at| at + 1);
    let expected_column = source[line_start..anchor].chars().count() + 1;
    assert_eq!(
        error.span.column, expected_column,
        "column must count characters, not bytes: {error:?}"
    );
    Ok(())
}
