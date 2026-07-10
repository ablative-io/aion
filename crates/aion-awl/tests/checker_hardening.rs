//! Checker fix-round hardening (2026-07-11 adversarial panel), split from
//! `checker_regressions.rs` for file-size discipline:
//!
//! 4. Inline schema-door diagnostics anchor at the offending JSON path,
//!    not the first occurrence of the keyword token.
//! 8. Structural compatibility has no acceptance depth cap; recursive
//!    types still terminate coinductively.
//!
//! Advisory hardening: dead control flow (statements behind an
//! unconditional route, outcome clauses behind a body-terminal route) and
//! call-site config on child calls are refused.

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
// 4. Inline schema-door anchors are path-correct
// ---------------------------------------------------------------------

#[test]
fn inline_schema_error_anchors_at_the_offending_nested_token() -> TestResult {
    let source = "\
//! The root `\"type\"` repeats inside a nested property.
workflow nested_anchor
  input seed: String
  outcome done: type Out, route success

type Out { text: String }

type Weird = schema {
  \"type\": \"object\",
  \"properties\": {
    \"state\": { \"type\": \"frobnicate\" }
  }
}

worker w
  action make(text: String) -> Out

step only
  make(text: seed) -> out
  out |> route done
";
    let errors = check_source(source)?;
    let line = line_of(source, "\"type\": \"frobnicate\"")?;
    let error = find_error(&errors, "unsupported `type` `frobnicate`", line)?;
    assert!(
        error.message.contains("`state`"),
        "the diagnostic names the JSON path: {error:?}"
    );
    Ok(())
}

#[test]
fn inline_schema_refusal_skips_an_earlier_property_named_like_the_keyword() -> TestResult {
    let source = "\
//! A property literally named oneOf must not pull the anchor.
workflow keyword_named_property
  input seed: String
  outcome done: type Out, route success

type Out { text: String }

type Tricky = schema {
  \"type\": \"object\",
  \"properties\": {
    \"oneOf\": { \"type\": \"string\" },
    \"bad\":   { \"oneOf\": [ { \"type\": \"string\" } ] }
  }
}

worker w
  action make(text: String) -> Out

step only
  make(text: seed) -> out
  out |> route done
";
    let errors = check_source(source)?;
    let line = line_of(source, "\"bad\":")?;
    let error = find_error(&errors, "oneOf", line)?;
    assert!(
        error.message.contains("`bad`"),
        "the diagnostic names the JSON path: {error:?}"
    );
    assert_eq!(
        errors.len(),
        1,
        "expected exactly one error; got {errors:#?}"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// 8. No structural-compatibility depth cap; recursion is coinductive
// ---------------------------------------------------------------------

fn deep_nesting_doc(levels: usize) -> String {
    use std::fmt::Write as _;
    let mut types = String::new();
    for index in 0..levels {
        let _ = writeln!(types, "type T{index} {{ v: T{} }}", index + 1);
        let _ = writeln!(types, "type U{index} {{ v: U{} }}", index + 1);
    }
    let _ = writeln!(types, "type T{levels} {{ v: Int }}");
    let _ = writeln!(types, "type U{levels} {{ v: String }}");
    format!(
        "//! Deeply nested record chains with a leaf mismatch.\n\
         workflow deep_mismatch\n\
         \x20 input seed: T0\n\
         \x20 outcome done: type Out, route success\n\
         \n\
         type Out {{ text: String }}\n\
         {types}\n\
         worker w\n\
         \x20 action go(x: U0) -> Out\n\
         \n\
         step only\n\
         \x20 go(x: seed) -> out\n\
         \x20 out |> route done\n"
    )
}

#[test]
fn deep_record_mismatch_is_refused_past_the_old_depth_cap() -> TestResult {
    let source = deep_nesting_doc(30);
    let errors = check_source(&source)?;
    let line = line_of(&source, "go(x: seed)")?;
    let error = find_error(&errors, "expects U0", line)?;
    assert!(
        error.message.contains("found T0"),
        "the mismatch names both types: {error:?}"
    );
    Ok(())
}

#[test]
fn structurally_equal_recursive_types_are_compatible_and_terminate() -> TestResult {
    let source = "\
//! Two recursive types with the same shape are interchangeable.
workflow recursive_ok
  input head: NodeA
  outcome done: type Out, route success

type Out   { text: String }
type NodeA { label: String, next: NodeA? }
type NodeB { label: String, next: NodeB? }

worker w
  action eat(node: NodeB) -> Out

step only
  eat(node: head) -> out
  out |> route done
";
    let errors = check_source(source)?;
    assert_eq!(errors, Vec::new(), "must check clean");
    Ok(())
}

// ---------------------------------------------------------------------
// Advisory hardening: dead control flow and child call-site config
// ---------------------------------------------------------------------

#[test]
fn statements_after_an_unconditional_route_are_refused() -> TestResult {
    let source = "\
//! Dead code behind a mid-body route.
workflow dead_tail
  input seed: String
  outcome done: type Out, route success

type Out { text: String }

worker w
  action make(text: String) -> Out

step first
  route second
  make(text: seed) -> dead

step second
  make(text: seed) -> out
  out |> route done
";
    let errors = check_source(source)?;
    let line = line_of(source, "make(text: seed) -> dead")?;
    find_error(&errors, "unreachable statement", line)?;
    Ok(())
}

#[test]
fn outcome_clauses_behind_a_body_terminal_route_are_refused() -> TestResult {
    let source = "\
//! Outcomes are evaluated after the body — which always routes away.
workflow dead_outcomes
  input seed: String
  outcome done: type Out, route success

type Out { text: String }

worker w
  action make(text: String) -> Out

step first
  make(text: seed) -> out
  route second

  outcome extra: when out.text == \"\", route done
  outcome more: otherwise, route done(text: out.text)

step second
  make(text: seed) -> fin
  fin |> route done
";
    let errors = check_source(source)?;
    let line = line_of(source, "outcome extra")?;
    find_error(&errors, "can never fire", line)?;
    Ok(())
}

#[test]
fn call_site_config_on_a_child_call_is_refused() -> TestResult {
    let source = "\
//! `node`/`timeout` pins apply to worker actions only.
workflow child_pin
  input seed: String
  outcome done: type Out, route success

type Out { text: String }

worker w
  action make(text: String) -> Out

child helper(brief: String) -> Out

step work
  helper(brief: seed) -> out
    node developer, timeout 5m
  out |> route done
";
    let errors = check_source(source)?;
    let line = line_of(source, "node developer")?;
    find_error(&errors, "child call carries no call-site config", line)?;
    Ok(())
}
