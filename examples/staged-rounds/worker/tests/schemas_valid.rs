//! The embedded agent output schemas must be valid JSON Schema documents —
//! Norn receives them inline via `--output-schema`, and a malformed
//! document would fail every agent turn at spawn time. Each schema also
//! validates a canonical sample payload and rejects a violating one.

use staged_rounds_worker::schemas;

fn validator(schema: &str) -> anyhow::Result<jsonschema::Validator> {
    let value: serde_json::Value = serde_json::from_str(schema)?;
    jsonschema::validator_for(&value)
        .map_err(|error| anyhow::anyhow!("schema does not build a validator: {error}"))
}

#[test]
fn every_embedded_schema_is_a_json_object_document() -> anyhow::Result<()> {
    for (name, schema) in [
        ("plan", schemas::PLAN),
        ("item-report", schemas::ITEM_REPORT),
        ("item-verdict", schemas::ITEM_VERDICT),
        ("remediation", schemas::REMEDIATION),
    ] {
        let value: serde_json::Value = serde_json::from_str(schema)
            .map_err(|error| anyhow::anyhow!("schema {name} is not valid JSON: {error}"))?;
        anyhow::ensure!(value.is_object(), "schema {name} must be a JSON object");
        anyhow::ensure!(
            value["type"] == "object",
            "schema {name} must constrain an object"
        );
        validator(schema).map_err(|error| anyhow::anyhow!("schema {name}: {error}"))?;
    }
    Ok(())
}

#[test]
fn the_plan_schema_accepts_a_canonical_plan_and_rejects_a_bad_slug() -> anyhow::Result<()> {
    let validator = validator(schemas::PLAN)?;
    let good = serde_json::json!({
        "summary": "two disjoint items",
        "items": [{
            "id": "split-core",
            "title": "Split the core",
            "goal": "Extract the parser into its own module.",
            "scope_in": ["src/parser.rs"],
            "scope_out": ["src/lib.rs"],
            "phase": 1,
            "depends_on": [],
            "feedback": ""
        }]
    });
    anyhow::ensure!(
        validator.is_valid(&good),
        "the canonical plan must validate"
    );

    let mut bad_slug = good.clone();
    bad_slug["items"][0]["id"] = serde_json::json!("Split_Core");
    anyhow::ensure!(
        !validator.is_valid(&bad_slug),
        "a non-slug id must fail the plan schema"
    );

    let mut bad_feedback = good;
    bad_feedback["items"][0]["feedback"] = serde_json::json!("prefilled");
    anyhow::ensure!(
        !validator.is_valid(&bad_feedback),
        "a planner-prefilled feedback must fail the plan schema"
    );
    Ok(())
}

#[test]
fn the_item_report_schema_accepts_a_canonical_report() -> anyhow::Result<()> {
    let validator = validator(schemas::ITEM_REPORT)?;
    let good = serde_json::json!({
        "item_id": "split-core",
        "summary": "Extracted the parser.",
        "commits": [],
        "claims": [{"criterion": "parser extracted", "how": "src/parser.rs added"}]
    });
    anyhow::ensure!(validator.is_valid(&good));
    let bad = serde_json::json!({ "summary": "no id" });
    anyhow::ensure!(!validator.is_valid(&bad));
    Ok(())
}

#[test]
fn the_item_verdict_schema_requires_a_reason_on_reject() -> anyhow::Result<()> {
    let validator = validator(schemas::ITEM_VERDICT)?;
    let accept = serde_json::json!({
        "item_id": "split-core",
        "overall": "accept",
        "findings": []
    });
    anyhow::ensure!(validator.is_valid(&accept));

    let reject_without_reason = serde_json::json!({
        "item_id": "split-core",
        "overall": "reject",
        "findings": [{"severity": "blocking", "title": "t", "evidence": "e"}]
    });
    anyhow::ensure!(
        !validator.is_valid(&reject_without_reason),
        "a rejecting verdict without reject_reason must fail"
    );

    let reject_with_reason = serde_json::json!({
        "item_id": "split-core",
        "overall": "reject",
        "findings": [{"severity": "blocking", "title": "t", "evidence": "e"}],
        "reject_reason": "wrong seam"
    });
    anyhow::ensure!(validator.is_valid(&reject_with_reason));
    Ok(())
}

#[test]
fn the_remediation_schema_accepts_a_canonical_report() -> anyhow::Result<()> {
    let validator = validator(schemas::REMEDIATION)?;
    let good = serde_json::json!({
        "summary": "kept both intents in shared.txt",
        "resolved": ["shared.txt"]
    });
    anyhow::ensure!(validator.is_valid(&good));
    let bad = serde_json::json!({ "resolved": [] });
    anyhow::ensure!(!validator.is_valid(&bad), "a summary is required");
    Ok(())
}
