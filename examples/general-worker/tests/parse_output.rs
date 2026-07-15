//! Complete deterministic-mode coverage for `parse_output`.

use std::error::Error;

use general_worker::{ParseInput, ParseOutput, parse_output};

type TestResult = Result<(), Box<dyn Error>>;

fn parse(text: &str, mode: &str, query: &str) -> ParseOutput {
    parse_output(ParseInput {
        text: text.to_owned(),
        mode: mode.to_owned(),
        query: query.to_owned(),
    })
}

fn success_value(output: ParseOutput) -> Result<String, Box<dyn Error>> {
    if !output.ok {
        return Err(format!("expected success, got error: {:?}", output.error).into());
    }
    output
        .value
        .ok_or_else(|| "successful parse must carry a value".into())
}

fn failure_error(output: ParseOutput) -> Result<String, Box<dyn Error>> {
    if output.ok {
        return Err(format!("expected failure, got value: {:?}", output.value).into());
    }
    output
        .error
        .ok_or_else(|| "failed parse must carry an error".into())
}

#[test]
fn json_path_traverses_objects_arrays_and_renders_scalar_types() -> TestResult {
    let text = r#"{"items":[{"name":"alpha","count":7,"ready":true,"none":null}]}"#;
    assert_eq!(
        success_value(parse(text, "json_path", "items.0.name"))?,
        "alpha"
    );
    assert_eq!(
        success_value(parse(text, "json_path", "items.0.count"))?,
        "7"
    );
    assert_eq!(
        success_value(parse(text, "json_path", "items.0.ready"))?,
        "true"
    );
    assert_eq!(
        success_value(parse(text, "json_path", "items.0.none"))?,
        "null"
    );
    Ok(())
}

#[test]
fn json_path_empty_query_addresses_compact_root() -> TestResult {
    assert_eq!(
        success_value(parse(
            r#"{"alpha":[1,2],"beta":{"ok":true}}"#,
            "json_path",
            ""
        ))?,
        r#"{"alpha":[1,2],"beta":{"ok":true}}"#
    );
    Ok(())
}

#[test]
fn json_path_reports_malformed_json_and_precise_path_failures() -> TestResult {
    let malformed = failure_error(parse("{broken", "json_path", "value"))?;
    assert!(malformed.contains("failed to parse input JSON"));

    assert_eq!(
        failure_error(parse(r#"{"items":[]}"#, "json_path", "items.first"))?,
        "json_path segment `first` is not a numeric array index at segment 2: invalid digit found in string"
    );
    assert_eq!(
        failure_error(parse(r#"{"items":[]}"#, "json_path", "items.0"))?,
        "json_path array index 0 is out of bounds at segment 2"
    );
    assert_eq!(
        failure_error(parse(r#"{"item":1}"#, "json_path", "missing"))?,
        "json_path key `missing` was not found at segment 1"
    );
    assert_eq!(
        failure_error(parse(r#"{"item":1}"#, "json_path", "item.next"))?,
        "json_path cannot traverse segment `next` through a number at segment 2"
    );
    assert_eq!(
        failure_error(parse(r#"{"item":1}"#, "json_path", "item..next"))?,
        "json_path segment 2 is empty in query `item..next`"
    );
    Ok(())
}

#[test]
fn regex_named_captures_are_a_deterministically_ordered_object() -> TestResult {
    let output = parse(
        "ticket=GPW-1 owner=tom",
        "regex",
        r"ticket=(?P<z>[A-Z]+-\d+) owner=(?P<a>\w+)(?: note=(?P<note>\w+))?",
    );
    assert_eq!(
        success_value(output)?,
        r#"{"a":"tom","note":null,"z":"GPW-1"}"#
    );
    Ok(())
}

#[test]
fn regex_positional_captures_return_an_array() -> TestResult {
    assert_eq!(
        success_value(parse("version=12.34", "regex", r"version=(\d+)\.(\d+)"))?,
        r#"["12","34"]"#
    );
    Ok(())
}

#[test]
fn regex_without_explicit_groups_returns_the_full_match() -> TestResult {
    assert_eq!(
        success_value(parse("prefix GPW-1 suffix", "regex", r"GPW-\d+"))?,
        r#"["GPW-1"]"#
    );
    Ok(())
}

#[test]
fn regex_compile_error_and_miss_are_nonterminal_failures() -> TestResult {
    let compile = failure_error(parse("text", "regex", "("))?;
    assert!(compile.starts_with("regex failed to compile query `(`:"));
    assert_eq!(
        failure_error(parse("text", "regex", "absent"))?,
        "regex query `absent` did not match input"
    );
    Ok(())
}

#[test]
fn lines_returns_all_substring_matches_in_source_order() -> TestResult {
    assert_eq!(
        success_value(parse(
            "info start\nerror one\nwarning\nerror two\n",
            "lines",
            "error"
        ))?,
        "error one\nerror two"
    );
    Ok(())
}

#[test]
fn lines_miss_and_unsupported_mode_are_nonterminal_failures() -> TestResult {
    assert_eq!(
        failure_error(parse("one\ntwo", "lines", "absent"))?,
        "lines query `absent` matched no lines"
    );
    assert_eq!(
        failure_error(parse("text", "yaml", "x"))?,
        "unsupported parse_output mode `yaml`; expected `json_path`, `regex`, or `lines`"
    );
    Ok(())
}

#[test]
fn identical_inputs_produce_identical_outputs() -> TestResult {
    let first = parse("x=42", "regex", r"x=(\d+)");
    let second = parse("x=42", "regex", r"x=(\d+)");
    assert_eq!(first, second);
    let encoded = serde_json::to_string(&first)?;
    let decoded = serde_json::from_str::<ParseOutput>(&encoded)?;
    assert_eq!(decoded, first);
    Ok(())
}
