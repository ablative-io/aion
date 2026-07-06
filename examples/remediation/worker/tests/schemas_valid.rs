#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The four embedded `--output-schema` documents must be valid JSON objects
//! with the exact `required` field sets the Gleam output codecs decode. A
//! schema that drifts from its codec would let Norn return a shape the
//! workflow cannot decode — a silent contract break this test forbids.
//! (The copies under `schemas/` mirror yggdrasil's
//! `docs/design/remediation-flow/schemas/`; yggdrasil's are the source of
//! truth — see the example README.)

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

fn enum_values(schema: &Value) -> Vec<String> {
    schema["enum"]
        .as_array()
        .expect("enum array")
        .iter()
        .map(|value| value.as_str().expect("string").to_owned())
        .collect()
}

#[test]
fn test_manifest_schema_matches_the_codec_fields() {
    let schema = parse(
        "test-manifest.schema.json",
        remediation_worker::schemas::TEST_MANIFEST,
    );
    assert_eq!(required(&schema), vec!["brief_id", "entries"]);
    let entry_required = required(&schema["properties"]["entries"]["items"]);
    assert_eq!(
        entry_required,
        vec![
            "finding_id",
            "test_names",
            "fail_evidence",
            "could_not_reproduce"
        ]
    );
}

#[test]
fn fix_report_schema_matches_the_codec_fields() {
    let schema = parse(
        "fix-report.schema.json",
        remediation_worker::schemas::FIX_REPORT,
    );
    assert_eq!(
        required(&schema),
        vec![
            "brief_id",
            "commits",
            "findings_addressed",
            "deviations",
            "new_tests"
        ]
    );
    assert_eq!(
        required(&schema["properties"]["findings_addressed"]["items"]),
        vec!["finding_id", "how"]
    );
    assert_eq!(
        required(&schema["properties"]["deviations"]["items"]),
        vec!["what", "why", "approved_by"]
    );
}

#[test]
fn verdict_schema_matches_the_codec_fields_and_ruling_vocabulary() {
    let schema = parse("verdict.schema.json", remediation_worker::schemas::VERDICT);
    assert_eq!(
        required(&schema),
        vec!["brief_id", "per_finding", "class_siblings_found"]
    );
    let ruling_item = &schema["properties"]["per_finding"]["items"];
    assert_eq!(
        required(ruling_item),
        vec!["finding_id", "ruling", "evidence"]
    );
    // The exact ruling vocabulary the Gleam Ruling type decodes.
    assert_eq!(
        enum_values(&ruling_item["properties"]["ruling"]),
        vec!["fixed", "partial", "not_fixed", "regression_introduced"]
    );
    assert_eq!(
        required(&schema["properties"]["class_siblings_found"]["items"]),
        vec!["file", "line", "description"]
    );
}

#[test]
fn re_audit_findings_schema_matches_the_profile_contract() {
    let schema = parse(
        "re-audit-findings.schema.json",
        remediation_worker::schemas::RE_AUDIT_FINDINGS,
    );
    assert_eq!(required(&schema), vec!["findings", "area_summary"]);
    assert_eq!(
        required(&schema["properties"]["findings"]["items"]),
        vec![
            "title",
            "file",
            "line",
            "category",
            "severity",
            "detail",
            "failure_scenario",
            "recommendation"
        ]
    );
}

/// Every embedded schema is a closed contract: `additionalProperties: false`
/// at the top level, so a driven agent cannot smuggle extra fields past the
/// workflow's decoder.
#[test]
fn every_schema_is_a_closed_object() {
    for (name, raw) in [
        (
            "test-manifest.schema.json",
            remediation_worker::schemas::TEST_MANIFEST,
        ),
        (
            "fix-report.schema.json",
            remediation_worker::schemas::FIX_REPORT,
        ),
        ("verdict.schema.json", remediation_worker::schemas::VERDICT),
        (
            "re-audit-findings.schema.json",
            remediation_worker::schemas::RE_AUDIT_FINDINGS,
        ),
    ] {
        let schema = parse(name, raw);
        assert_eq!(
            schema["additionalProperties"],
            Value::Bool(false),
            "{name} must be a closed object"
        );
    }
}
