//! Manifest-driven, zero-source shell worker composition root.
//!
//! The manifest contains wiring only: commands, scalar argument/environment
//! projections, and a text-vs-JSON encoding hint derived from AWL. Types,
//! timeout, and retry remain owned by the `.awl`; output shape is enforced by
//! the workflow decoder exactly as it is for every other worker.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use aion_worker::{ActivityContext, ActivityFailure, Worker, WorkerConfig};
use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;

#[derive(Debug, Args)]
pub struct ShellArgs {
    /// Strict TOML shell-worker manifest generated from checked AWL.
    #[arg(long)]
    manifest: PathBuf,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellManifest {
    worker: WorkerSection,
    action: Vec<ActionWiring>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerSection {
    name: String,
    task_queue: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ResultEncoding {
    Text,
    Json,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionWiring {
    name: String,
    command: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    result: ResultEncoding,
}

pub async fn run(args: &ShellArgs, endpoint: &str) -> Result<()> {
    let source = std::fs::read_to_string(&args.manifest)
        .with_context(|| format!("failed to read shell manifest {}", args.manifest.display()))?;
    let manifest = parse_manifest(&source)?;
    build_worker(manifest, endpoint)?.run().await?;
    Ok(())
}

fn build_worker(manifest: ShellManifest, endpoint: &str) -> Result<Worker> {
    let config = WorkerConfig::builder()
        .endpoint(endpoint)
        .task_queue(&manifest.worker.task_queue)
        .identity(format!("{}-shell-worker", manifest.worker.name))
        .max_concurrency(4)
        .reconnect_initial_backoff(Duration::from_millis(100))
        .reconnect_max_backoff(Duration::from_secs(5))
        .reconnect_max_attempts(usize::MAX)
        .build()?;
    let mut builder = Worker::builder(config);
    for action in manifest.action {
        let name = action.name.clone();
        builder = builder.register_activity(name, move |input: Value, context| {
            let action = action.clone();
            Box::pin(async move { execute(&action, &input, context).await })
        })?;
    }
    builder.build().map_err(Into::into)
}

fn parse_manifest(source: &str) -> Result<ShellManifest> {
    let manifest: ShellManifest = toml_edit::de::from_str(source)
        .context("shell worker manifest is not valid strict TOML")?;
    if manifest.worker.name.trim().is_empty() {
        bail!("shell worker manifest worker.name must not be empty");
    }
    if manifest.worker.task_queue.trim().is_empty() {
        bail!("shell worker manifest worker.task_queue must not be empty");
    }
    if manifest.action.is_empty() {
        bail!("shell worker manifest must declare at least one action");
    }
    let mut names = BTreeSet::new();
    for action in &manifest.action {
        if action.name.trim().is_empty() {
            bail!("shell worker manifest action.name must not be empty");
        }
        if !names.insert(action.name.as_str()) {
            bail!("shell worker manifest repeats action `{}`", action.name);
        }
        if action.command.is_empty() || action.command[0].trim().is_empty() {
            bail!("shell worker action `{}` has an empty command", action.name);
        }
        for value in action.command.iter().chain(action.env.values()) {
            validate_placeholders(value)
                .with_context(|| format!("invalid projection for action `{}`", action.name))?;
        }
    }
    Ok(manifest)
}

fn validate_placeholders(value: &str) -> Result<()> {
    let mut rest = value;
    while let Some(start) = rest.find('{') {
        let after = &rest[start..];
        let Some(end) = after.find('}') else {
            bail!("unterminated placeholder in `{value}`");
        };
        let placeholder = &after[..=end];
        if placeholder != "{input}"
            && !(placeholder.starts_with("{input.")
                && placeholder.len() > "{input.}".len()
                && placeholder[7..placeholder.len() - 1]
                    .chars()
                    .all(|character| character == '_' || character.is_ascii_alphanumeric()))
        {
            bail!("unsupported placeholder `{placeholder}`");
        }
        rest = &after[end + 1..];
    }
    if rest.contains('}') {
        bail!("unmatched closing brace in `{value}`");
    }
    Ok(())
}

async fn execute(
    action: &ActionWiring,
    input: &Value,
    context: &ActivityContext,
) -> Result<Value, ActivityFailure> {
    let program = expand(&action.command[0], input)?;
    let mut command = Command::new(program);
    command.kill_on_drop(true);
    for argument in &action.command[1..] {
        command.arg(expand(argument, input)?);
    }
    for (name, value) in &action.env {
        command.env(name, expand(value, input)?);
    }
    let output = tokio::select! {
        result = command.output() => result.map_err(|error| {
            ActivityFailure::retryable(format!("shell action `{}` could not spawn: {error}", action.name))
        })?,
        () = context.cancelled() => {
            return Err(ActivityFailure::terminal(format!("shell action `{}` was cancelled", action.name)));
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !output.status.success() {
        let exit = output
            .status
            .code()
            .map_or_else(|| "signal".to_owned(), |code| code.to_string());
        return Err(ActivityFailure::retryable(format!(
            "shell action `{}` exited {exit}: {stderr}",
            action.name
        )));
    }
    match action.result {
        ResultEncoding::Text => Ok(Value::String(stdout)),
        ResultEncoding::Json => serde_json::from_str(&stdout).map_err(|error| {
            ActivityFailure::terminal(format!(
                "shell action `{}` emitted invalid JSON: {error}",
                action.name
            ))
        }),
    }
}

fn expand(template: &str, input: &Value) -> Result<String, ActivityFailure> {
    let whole = serde_json::to_string(input).map_err(|error| {
        ActivityFailure::terminal(format!("input could not be projected as JSON: {error}"))
    })?;
    let mut output = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        output.push_str(&rest[..start]);
        let after = &rest[start..];
        let end = after.find('}').ok_or_else(|| {
            ActivityFailure::terminal(format!("unterminated input placeholder in `{template}`"))
        })?;
        let placeholder = &after[..=end];
        if placeholder == "{input}" {
            output.push_str(&whole);
        } else if let Some(field) = placeholder
            .strip_prefix("{input.")
            .and_then(|value| value.strip_suffix('}'))
        {
            let value = input.get(field).ok_or_else(|| {
                ActivityFailure::terminal(format!("input has no top-level field `{field}`"))
            })?;
            output.push_str(&scalar(field, value)?);
        } else {
            return Err(ActivityFailure::terminal(format!(
                "unsupported input placeholder `{placeholder}`"
            )));
        }
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

fn scalar(field: &str, value: &Value) -> Result<String, ActivityFailure> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => {
            Err(ActivityFailure::terminal(format!(
                "input field `{field}` is composite or optional and cannot be projected into an argument or environment value"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use aion_core::ActivityId;
    use serde_json::json;

    use super::*;

    const MANIFEST: &str = r#"
[worker]
name = "greeter"
task_queue = "greeter"

[[action]]
name = "greet"
command = ["printf", "%s", "{input.name}"]
result = "text"
"#;

    #[test]
    fn strict_manifest_rejects_unknown_keys_and_bad_placeholders() {
        assert!(
            parse_manifest(
                &MANIFEST.replace("name = \"greeter\"", "name = \"greeter\"\nextra = true")
            )
            .is_err()
        );
        assert!(parse_manifest(&MANIFEST.replace("{input.name}", "{nested.name}")).is_err());
    }

    #[test]
    fn composite_field_projection_is_typed_refusal() {
        let failure = expand("{input.items}", &json!({"items": [1, 2]}));
        assert!(failure.is_err());
    }

    #[tokio::test]
    async fn process_boundary_round_trips_text_and_json() -> Result<()> {
        let manifest = parse_manifest(MANIFEST)?;
        let worker = build_worker(manifest.clone(), "http://127.0.0.1:50051")?;
        assert_eq!(worker.activity_types(), &["greet"]);
        let (context, _cancellation) =
            ActivityContext::new(ActivityId::from_sequence_position(1), 1);
        let text = execute(&manifest.action[0], &json!({"name": "Ada"}), &context).await?;
        assert_eq!(text, json!("Ada"));

        let json_action = ActionWiring {
            name: "record".to_owned(),
            command: vec!["printf".to_owned(), "%s".to_owned(), "{input}".to_owned()],
            env: BTreeMap::new(),
            result: ResultEncoding::Json,
        };
        let value = execute(&json_action, &json!({"ok": true}), &context).await?;
        assert_eq!(value, json!({"ok": true}));
        Ok(())
    }
}
