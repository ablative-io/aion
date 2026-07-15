//! Exact `run_agent` Norn preparation and protocol-validation tests.

use std::error::Error;

use aion_integrations::{ActivityId, AgentRunSpec, HarnessError, Payload, WorkflowId};
use general_worker::GeneralNornHarness;
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn Error>>;

fn run_spec(input: &Value) -> Result<AgentRunSpec, Box<dyn Error>> {
    Ok(AgentRunSpec::new(
        WorkflowId::new_v4(),
        ActivityId::from_sequence_position(3),
        2,
        "run_agent",
        Payload::from_json(input)?,
    ))
}

#[test]
fn prepares_all_static_and_per_run_arguments_with_default_session() -> TestResult {
    let harness = GeneralNornHarness::new("norn-test");
    let prepared = harness.prepare_run(run_spec(&json!({
        "instructions": "Be exact and return only the schema.",
        "prompt": "Inspect the workspace and summarize it.",
        "output_schema": "{\"type\":\"object\"}",
        "workspace_path": "/work/repo",
        "disallowed_tools": "write,edit,apply_patch"
    }))?)?;

    assert_eq!(
        prepared.argv,
        vec![
            "--protocol",
            "jsonrpc",
            "--fast",
            "--reasoning-effort",
            "high",
            "--append-system-prompt",
            "Be exact and return only the schema.",
            "--output-schema",
            "{\"type\":\"object\"}",
            "--session-id",
            "{workflow_id}-agent",
            "--resume-if-exists",
            "--workspace-root",
            "/work/repo",
            "--disallowed-tools",
            "write,edit,apply_patch",
        ]
    );
    assert_eq!(prepared.removed_environment, vec!["OPENAI_API_KEY"]);
    assert_eq!(
        prepared.spec.input.to_json()?,
        Value::String("Inspect the workspace and summarize it.".to_owned())
    );
    Ok(())
}

#[test]
fn supplied_session_and_absent_deny_list_replace_only_the_optional_arguments() -> TestResult {
    let harness = GeneralNornHarness::new("norn-test");
    let prepared = harness.prepare_run(run_spec(&json!({
        "instructions": "Use the supplied session.",
        "prompt": "Continue the prior analysis.",
        "output_schema": "{\"type\":\"string\"}",
        "session_key": "shared-review-session",
        "workspace_path": "/work/other"
    }))?)?;

    let expected_tail: Vec<String> = [
        "--append-system-prompt",
        "Use the supplied session.",
        "--output-schema",
        "{\"type\":\"string\"}",
        "--session-id",
        "shared-review-session",
        "--resume-if-exists",
        "--workspace-root",
        "/work/other",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert!(prepared.argv.ends_with(&expected_tail));
    assert!(!prepared.argv.iter().any(|arg| arg == "--disallowed-tools"));
    assert_eq!(
        prepared.spec.input.to_json()?,
        Value::String("Continue the prior analysis.".to_owned())
    );
    Ok(())
}

#[test]
fn missing_and_blank_required_values_are_protocol_errors() -> TestResult {
    let harness = GeneralNornHarness::new("norn-test");
    let missing = harness
        .prepare_run(run_spec(&json!({
            "prompt": "hello",
            "output_schema": "{}",
            "workspace_path": "/work"
        }))?)
        .err()
        .ok_or("missing instructions must fail")?;
    assert!(matches!(missing, HarnessError::Protocol { .. }));
    assert!(missing.to_string().contains("missing field `instructions`"));

    for (field, input) in [
        (
            "instructions",
            json!({
                "instructions": "  ", "prompt": "hello", "output_schema": "{}",
                "workspace_path": "/work"
            }),
        ),
        (
            "prompt",
            json!({
                "instructions": "do it", "prompt": "\n", "output_schema": "{}",
                "workspace_path": "/work"
            }),
        ),
        (
            "output_schema",
            json!({
                "instructions": "do it", "prompt": "hello", "output_schema": "",
                "workspace_path": "/work"
            }),
        ),
        (
            "workspace_path",
            json!({
                "instructions": "do it", "prompt": "hello", "output_schema": "{}",
                "workspace_path": "\t"
            }),
        ),
    ] {
        let failure = harness
            .prepare_run(run_spec(&input)?)
            .err()
            .ok_or_else(|| format!("blank {field} must fail"))?;
        assert!(matches!(failure, HarnessError::Protocol { .. }));
        assert!(failure.to_string().contains(field));
        assert!(failure.to_string().contains("nonblank"));
    }
    Ok(())
}

#[test]
fn blank_supplied_session_is_a_protocol_error() -> TestResult {
    let harness = GeneralNornHarness::new("norn-test");
    let failure = harness
        .prepare_run(run_spec(&json!({
            "instructions": "do it",
            "prompt": "hello",
            "output_schema": "{}",
            "session_key": " ",
            "workspace_path": "/work"
        }))?)
        .err()
        .ok_or("blank supplied session must fail")?;
    assert!(matches!(failure, HarnessError::Protocol { .. }));
    assert!(failure.to_string().contains("session_key"));
    Ok(())
}
