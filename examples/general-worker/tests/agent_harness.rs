//! Exact `run_agent` Norn preparation and protocol-validation tests.

use std::error::Error;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;

use aion_integrations::{ActivityId, AgentRunSpec, HarnessError, Payload, WorkflowId};
#[cfg(unix)]
use aion_integrations::{AgentHarness, AgentSession};
use general_worker::GeneralNornHarness;
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn Error>>;

#[cfg(unix)]
const FAKE_NORN_CHILD_ROOT: &str = "GENERAL_WORKER_FAKE_NORN_CHILD_ROOT";
#[cfg(unix)]
const FAKE_NORN_TEST_NAME: &str =
    "fake_norn_executable_exercises_the_actual_start_and_session_path";

#[cfg(unix)]
fn local_tempdir(prefix: &str) -> Result<tempfile::TempDir, Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-temp");
    std::fs::create_dir_all(&root)?;
    Ok(tempfile::Builder::new().prefix(prefix).tempdir_in(root)?)
}

fn run_spec(input: &Value) -> Result<AgentRunSpec, Box<dyn Error>> {
    Ok(AgentRunSpec::new(
        WorkflowId::new_v4(),
        ActivityId::from_sequence_position(3),
        2,
        "run_agent",
        Payload::from_json(input)?,
    ))
}

#[cfg(unix)]
fn trace_path(executable: &Path, suffix: &str) -> PathBuf {
    let mut path = executable.as_os_str().to_os_string();
    path.push(format!(".{suffix}"));
    PathBuf::from(path)
}

#[cfg(unix)]
fn write_fake_norn(root: &Path) -> Result<PathBuf, Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let executable = root.join("fake-norn");
    let script = r#"#!/bin/sh
set -eu
printf '%s\n' "$@" > "$0.argv"
if [ "${OPENAI_API_KEY+x}" = x ]; then
  printf 'present\n' > "$0.openai-api-key"
else
  printf 'absent\n' > "$0.openai-api-key"
fi
IFS= read -r initialize
printf '%s\n' "$initialize" > "$0.initialize"
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocol":"norn-driven/1","capabilities":{"interventions":["inject_message","cancel"]}}}'
IFS= read -r run
printf '%s\n' "$run" > "$0.run"
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"envelope_version":1,"stop":{"reason":"completed"},"output":{"summary":"verbatim","items":[1,{"nested":true}]}}}'
"#;
    std::fs::write(&executable, script)?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
    Ok(executable)
}

#[cfg(unix)]
fn exercise_fake_norn(root: &Path) -> TestResult {
    let executable = write_fake_norn(root)?;
    let instructions = "Use literal {workflow_id} and {activity_type} instructions.";
    let prompt = "Return {workflow_id} and {activity_type} in the fake terminal object.";
    let schema = concat!(
        "  \n\t",
        r#"{"type":"object","$comment":"literal {workflow_id} {activity_type}","properties":{"summary":{"type":"string"},"items":{"type":"array"}},"required":["summary","items"]}"#,
        " \t"
    );
    let transported_schema = schema.trim_start();
    let session_key = "session-{workflow_id}-{activity_type}";
    let workspace = root
        .join("workspace-{workflow_id}-{activity_type}")
        .to_string_lossy()
        .into_owned();
    let disallowed_tools = "write,{workflow_id},{activity_type}";
    let spec = run_spec(&json!({
        "instructions": instructions,
        "prompt": prompt,
        "output_schema": schema,
        "session_key": session_key,
        "workspace_path": workspace,
        "disallowed_tools": disallowed_tools
    }))?;
    let runtime = tokio::runtime::Runtime::new()?;
    let payload = runtime.block_on(async {
        let session = GeneralNornHarness::new(&executable).start(spec).await?;
        session.wait_result().await
    })?;

    let captured_argv = std::fs::read_to_string(trace_path(&executable, "argv"))?;
    let argv: Vec<&str> = captured_argv.lines().collect();
    assert_eq!(
        argv,
        vec![
            "--protocol",
            "jsonrpc",
            "--fast",
            "--reasoning-effort",
            "high",
            "--append-system-prompt",
            instructions,
            "--output-schema",
            transported_schema,
            "--session-id",
            session_key,
            "--resume-if-exists",
            "--workspace-root",
            workspace.as_str(),
            "--disallowed-tools",
            disallowed_tools,
        ]
    );
    assert_eq!(
        std::fs::read_to_string(trace_path(&executable, "openai-api-key"))?,
        "absent\n"
    );

    let initialize: Value = serde_json::from_str(&std::fs::read_to_string(trace_path(
        &executable,
        "initialize",
    ))?)?;
    assert_eq!(initialize["method"], "initialize");
    let run: Value =
        serde_json::from_str(&std::fs::read_to_string(trace_path(&executable, "run"))?)?;
    assert_eq!(run["method"], "run/execute");
    assert_eq!(run["params"]["prompt"], prompt);
    assert_eq!(
        payload.to_json()?,
        json!({
            "summary": "verbatim",
            "items": [1, {"nested": true}],
        })
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn fake_norn_executable_exercises_the_actual_start_and_session_path() -> TestResult {
    if let Some(root) = std::env::var_os(FAKE_NORN_CHILD_ROOT) {
        return exercise_fake_norn(Path::new(&root));
    }

    let directory = local_tempdir("fake-norn-")?;
    let output = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg(FAKE_NORN_TEST_NAME)
        .arg("--nocapture")
        .env(FAKE_NORN_CHILD_ROOT, directory.path())
        .env("OPENAI_API_KEY", "ambient-key-must-not-reach-norn")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "fake Norn child test failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let executable = directory.path().join("fake-norn");
    if !trace_path(&executable, "argv").is_file() {
        return Err("fake Norn child test did not execute the protocol peer".into());
    }
    Ok(())
}

#[test]
fn prepares_all_static_and_per_run_arguments_with_default_session() -> TestResult {
    let harness = GeneralNornHarness::new("norn-test");
    let schema = "  \n\t{\"type\":\"object\",\"$comment\":\"{workflow_id} {activity_type}\"} \t";
    let spec = run_spec(&json!({
        "instructions": "Be literal: {workflow_id} and {activity_type}.",
        "prompt": "Inspect {workflow_id} and {activity_type} exactly.",
        "output_schema": schema,
        "workspace_path": "/work/{workflow_id}/{activity_type}",
        "disallowed_tools": "write,{workflow_id},{activity_type}"
    }))?;
    let expected_session = format!("{}-agent", spec.workflow_id);
    let prepared = harness.prepare_run(spec)?;

    assert_eq!(
        prepared.argv,
        vec![
            "--protocol",
            "jsonrpc",
            "--fast",
            "--reasoning-effort",
            "high",
            "--append-system-prompt",
            "Be literal: {workflow_id} and {activity_type}.",
            "--output-schema",
            "{\"type\":\"object\",\"$comment\":\"{workflow_id} {activity_type}\"} \t",
            "--session-id",
            expected_session.as_str(),
            "--resume-if-exists",
            "--workspace-root",
            "/work/{workflow_id}/{activity_type}",
            "--disallowed-tools",
            "write,{workflow_id},{activity_type}",
        ]
    );
    assert_eq!(prepared.removed_environment, vec!["OPENAI_API_KEY"]);
    assert_eq!(
        prepared.spec.input.to_json()?,
        Value::String("Inspect {workflow_id} and {activity_type} exactly.".to_owned())
    );
    Ok(())
}

#[test]
fn supplied_session_and_absent_deny_list_replace_only_the_optional_arguments() -> TestResult {
    let harness = GeneralNornHarness::new("norn-test");
    let prepared = harness.prepare_run(run_spec(&json!({
        "instructions": "Use supplied {workflow_id} and {activity_type} literally.",
        "prompt": "Continue {workflow_id} and {activity_type} literally.",
        "output_schema": "{\"type\":\"string\",\"$comment\":\"{workflow_id} {activity_type}\"}",
        "session_key": "shared-{workflow_id}-{activity_type}",
        "workspace_path": "/work/{workflow_id}/{activity_type}"
    }))?)?;

    let expected_tail: Vec<String> = [
        "--append-system-prompt",
        "Use supplied {workflow_id} and {activity_type} literally.",
        "--output-schema",
        "{\"type\":\"string\",\"$comment\":\"{workflow_id} {activity_type}\"}",
        "--session-id",
        "shared-{workflow_id}-{activity_type}",
        "--resume-if-exists",
        "--workspace-root",
        "/work/{workflow_id}/{activity_type}",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert!(prepared.argv.ends_with(&expected_tail));
    assert!(!prepared.argv.iter().any(|arg| arg == "--disallowed-tools"));
    assert_eq!(
        prepared.spec.input.to_json()?,
        Value::String("Continue {workflow_id} and {activity_type} literally.".to_owned())
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
