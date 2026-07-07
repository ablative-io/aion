//! The embedded agent output schemas must be valid JSON objects — Norn
//! receives them inline via `--output-schema`, and a malformed document
//! would fail every agent turn at spawn time.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use dev_brief_worker::schemas;

#[test]
fn every_embedded_schema_is_a_json_object_document() {
    for (name, schema) in [
        ("dev-report", schemas::DEV_REPORT),
        ("lens-verdict", schemas::LENS_VERDICT),
    ] {
        let value: serde_json::Value = serde_json::from_str(schema)
            .unwrap_or_else(|error| panic!("schema {name} is not valid JSON: {error}"));
        assert!(value.is_object(), "schema {name} must be a JSON object");
        assert_eq!(
            value["type"], "object",
            "schema {name} must constrain an object"
        );
    }
}

#[test]
fn the_lens_verdict_schema_requires_a_reason_on_reject() {
    let value: serde_json::Value = serde_json::from_str(schemas::LENS_VERDICT).expect("json");
    // The conditional requirement is the derive-and-check rule's schema-level
    // half: a rejecting verdict must carry reject_reason.
    assert_eq!(value["if"]["properties"]["overall"]["const"], "reject");
    assert!(
        value["then"]["required"]
            .as_array()
            .expect("required array")
            .iter()
            .any(|entry| entry == "reject_reason")
    );
}
