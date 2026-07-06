#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The four embedded `--output-schema` documents must be valid JSON objects
//! with the exact `required` field sets the Gleam output codecs decode. A schema
//! that drifts from its codec would let Norn return a shape the workflow cannot
//! decode — a silent contract break this test forbids.

use serde_json::Value;

fn parse(name: &str, raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|error| panic!("{name} is not valid JSON: {error}"))
}

fn required(schema: &Value) -> Vec<String> {
    schema["required"]
        .as_array()
        .expect("required array")
        .iter()
        .map(|value| value.as_str().expect("string").to_owned())
        .collect()
}

#[test]
fn scout_schema_matches_the_codec_fields() {
    let schema = parse(
        "scout_output.json",
        pipeline_run_worker::schemas::SCOUT_OUTPUT,
    );
    assert_eq!(
        required(&schema),
        vec![
            "summary",
            "observations",
            "integration_points",
            "risks",
            "not_covered"
        ],
    );
}

#[test]
fn stack_plan_schema_matches_the_codec_fields() {
    let schema = parse("stack_plan.json", pipeline_run_worker::schemas::STACK_PLAN);
    assert_eq!(required(&schema), vec!["units", "summary", "not_covered"]);
    // Each unit carries the four stratification-relevant fields.
    let unit_required = required(&schema["properties"]["units"]["items"]);
    assert_eq!(
        unit_required,
        vec!["unit_id", "goal", "files_hint", "depends_on"]
    );
}

#[test]
fn dev_schema_matches_the_codec_fields() {
    let schema = parse("dev_output.json", pipeline_run_worker::schemas::DEV_OUTPUT);
    assert_eq!(
        required(&schema),
        vec!["files_touched", "summary", "not_covered"]
    );
}

#[test]
fn review_schema_matches_the_codec_fields() {
    let schema = parse(
        "review_output.json",
        pipeline_run_worker::schemas::REVIEW_OUTPUT,
    );
    assert_eq!(
        required(&schema),
        vec!["pass", "blockers", "should_fix", "summary", "not_covered"],
    );
    // Every blocker must carry file:line evidence + problem + scenario.
    let blocker_required = required(&schema["properties"]["blockers"]["items"]);
    assert_eq!(blocker_required, vec!["evidence", "problem", "scenario"]);
}
