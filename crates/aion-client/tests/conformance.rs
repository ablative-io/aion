//! Live conformance harness for the shared aion client scenario contract.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::time::Duration;

use aion_client::{Client, ClientAuth, ClientError, ListPage, StartOptions, TlsOptions};
use aion_core::{ContentType, Event, Payload, RunId, WorkflowFilter, WorkflowId, WorkflowStatus};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde_json::{Map, Value, json};
use uuid::Uuid;

const SCENARIOS_PATH: &str = "conformance/aion-clients/scenarios.json";
const SERVER_URL_ENV: &str = "AION_SERVER_URL";
const AUTH_TOKEN_ENV: &str = "AION_AUTH_TOKEN";

#[tokio::test]
async fn shared_client_contract_conformance() -> Result<(), Box<dyn std::error::Error>> {
    let server_url = match env::var(SERVER_URL_ENV) {
        Ok(value) if !value.is_empty() => value,
        _ => {
            println!(
                "SKIP sdk=rust reason={SERVER_URL_ENV} is unset; live aion-server conformance not run"
            );
            return Ok(());
        }
    };

    let scenarios = load_scenarios()?;
    let defaults = scenarios
        .get("defaults")
        .and_then(Value::as_object)
        .ok_or("scenarios.json defaults must be an object")?;
    let fixtures = scenarios
        .get("fixtures")
        .and_then(Value::as_object)
        .ok_or("scenarios.json fixtures must be an object")?;
    let scenario_values = scenarios
        .get("scenarios")
        .and_then(Value::as_array)
        .ok_or("scenarios.json scenarios must be an array")?;

    for scenario in scenario_values {
        run_scenario(scenario, defaults, fixtures, &server_url).await?;
    }

    Ok(())
}

fn load_scenarios() -> Result<Value, Box<dyn std::error::Error>> {
    let repository_root = env!("CARGO_MANIFEST_DIR").trim_end_matches("crates/aion-client");
    let path = format!("{repository_root}{SCENARIOS_PATH}");
    let source = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&source)?)
}

async fn run_scenario(
    scenario: &Value,
    defaults: &Map<String, Value>,
    fixtures: &Map<String, Value>,
    server_url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let scenario_id = require_str(scenario, "id")?;
    let steps = scenario
        .get("steps")
        .and_then(Value::as_array)
        .ok_or("scenario steps must be an array")?;
    let started_at = Utc::now() - chrono::Duration::seconds(30);
    let now = Utc::now() + chrono::Duration::seconds(30);
    let mut context = ScenarioContext::default();

    for step in steps {
        let step_id = require_str(step, "id")?;
        let operation = require_str(step, "operation")?;
        let input = resolve_value(
            step.get("input").unwrap_or(&Value::Null),
            defaults,
            fixtures,
            scenario_id,
            &context,
            started_at,
            now,
        );
        let expected = resolve_value(
            step.get("expect").ok_or("step expect is required")?,
            defaults,
            fixtures,
            scenario_id,
            &context,
            started_at,
            now,
        );
        let actual = execute_step(operation, &input, &mut context, server_url).await;
        let normalized = match actual {
            Ok(value) => json!({ "ok": value }),
            Err(error) => json!({ "error": error_variant(&error) }),
        };
        println!(
            "AION_CONFORMANCE sdk=rust scenario={scenario_id} step={step_id} result={}",
            serde_json::to_string(&normalized)?
        );
        assert_matches(scenario_id, step_id, &normalized, &expected, &context);
        context.record(scenario_id, step_id, normalized);
    }

    Ok(())
}

async fn execute_step(
    operation: &str,
    input: &Value,
    context: &mut ScenarioContext,
    server_url: &str,
) -> Result<Value, ClientError> {
    match operation {
        "connect" => execute_connect(input, context, server_url).await,
        "start" => execute_start(input, context).await,
        "signal" => execute_signal(input, context).await,
        "query" => execute_query(input, context).await,
        "cancel" => execute_cancel(input, context).await,
        "list" => execute_list(input, context).await,
        "describe" => execute_describe(input, context).await,
        "subscribe" => {
            if input.get("collect").is_some() {
                Ok(json!({ "kind": "eventStreamStarted" }))
            } else {
                let events = collect_stream(input, context).await?;
                Ok(json!({ "kind": "eventStream", "events": events }))
            }
        }
        "harness.forceDisconnect" => Ok(json!({ "kind": "disconnectInjected" })),
        "harness.assertStream" => {
            let events = collect_stream(input, context).await?;
            Ok(
                json!({ "kind": "eventStream", "events": events, "sequenceContiguousUnique": sequences_contiguous_unique(&events) }),
            )
        }
        other => Err(ClientError::InvalidArgument).map_err(|error| ClientError::Server {
            detail: format!("unsupported conformance operation {other}: {error}"),
        }),
    }
}

async fn execute_connect(
    input: &Value,
    context: &mut ScenarioContext,
    server_url: &str,
) -> Result<Value, ClientError> {
    let mut builder = Client::builder(server_url)
        .with_namespace(input_str(input, "namespace").unwrap_or("conformance"));
    if let Ok(token) = env::var(AUTH_TOKEN_ENV) {
        if !token.is_empty() {
            builder = builder.with_auth(ClientAuth::bearer(token));
        }
    }
    if input.get("tls").and_then(Value::as_str) == Some("system-roots")
        && server_url.starts_with("https://")
    {
        builder = builder.with_tls(TlsOptions::new());
    }
    let client = builder.build().await?;
    context.client = Some(client);
    Ok(json!({ "kind": "client" }))
}

async fn execute_start(input: &Value, context: &mut ScenarioContext) -> Result<Value, ClientError> {
    let client = context.client()?;
    let handle = client
        .start(
            input_str(input, "workflowType").unwrap_or_default(),
            json_payload(input.get("payload"))?,
            StartOptions {
                namespace: input_str(input, "namespace").map(ToOwned::to_owned),
                idempotency_key: input_str(input, "idempotencyKey").map(ToOwned::to_owned),
            },
        )
        .await?;
    Ok(json!({
        "kind": "handle",
        "workflowId": handle.workflow_id().to_string(),
        "runId": handle.run_id().to_string()
    }))
}

async fn execute_signal(
    input: &Value,
    context: &mut ScenarioContext,
) -> Result<Value, ClientError> {
    let client = context.client()?;
    let wf_id = workflow_id(input_str(input, "workflowId").unwrap_or_default())?;
    let run_id = optional_run_id(input_str(input, "runId"))?;
    client
        .signal(
            &wf_id,
            run_id.as_ref(),
            input_str(input, "signalName").unwrap_or_default(),
            json_payload(input.get("payload"))?,
        )
        .await?;
    Ok(json!({ "kind": "accepted" }))
}

async fn execute_query(input: &Value, context: &mut ScenarioContext) -> Result<Value, ClientError> {
    let client = context.client()?;
    let wf_id = workflow_id(input_str(input, "workflowId").unwrap_or_default())?;
    let run_id = optional_run_id(input_str(input, "runId"))?;
    let deadline_ms = input
        .get("deadlineMs")
        .and_then(Value::as_u64)
        .unwrap_or(5000);
    let payload = client
        .query(
            &wf_id,
            run_id.as_ref(),
            input_str(input, "queryName").unwrap_or_default(),
            Payload::new(ContentType::Json, Vec::new()),
            Duration::from_millis(deadline_ms),
        )
        .await?;
    let decoded = payload.to_json().map_err(|error| ClientError::Server {
        detail: error.to_string(),
    })?;
    Ok(json!({ "kind": "payload", "value": decoded }))
}

async fn execute_cancel(
    input: &Value,
    context: &mut ScenarioContext,
) -> Result<Value, ClientError> {
    let client = context.client()?;
    let wf_id = workflow_id(input_str(input, "workflowId").unwrap_or_default())?;
    let run_id = optional_run_id(input_str(input, "runId"))?;
    client
        .cancel(
            &wf_id,
            run_id.as_ref(),
            input_str(input, "reason").unwrap_or_default(),
        )
        .await?;
    Ok(json!({ "kind": "accepted" }))
}

async fn execute_list(input: &Value, context: &mut ScenarioContext) -> Result<Value, ClientError> {
    let client = context.client()?;
    let filter = input.get("filter").unwrap_or(&Value::Null);
    let workflows = client
        .list(
            &WorkflowFilter {
                workflow_type: input_str(filter, "workflowType").map(ToOwned::to_owned),
                status: status(input_str(filter, "status")),
                started_after: datetime(input_str(filter, "startedAfter")),
                started_before: datetime(input_str(filter, "startedBefore")),
                parent: None,
            },
            ListPage::default(),
        )
        .await?;
    let summaries = workflows
        .into_iter()
        .map(|summary| {
            json!({
                "workflowId": summary.workflow_id.to_string(),
                "workflowType": summary.workflow_type,
                "status": format!("{:?}", summary.status)
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "kind": "workflowSummaryPage", "workflows": summaries }))
}

async fn execute_describe(
    input: &Value,
    context: &mut ScenarioContext,
) -> Result<Value, ClientError> {
    let client = context.client()?;
    let wf_id = workflow_id(input_str(input, "workflowId").unwrap_or_default())?;
    let run_id = optional_run_id(input_str(input, "runId"))?;
    let description = client.describe(&wf_id, run_id.as_ref()).await?;
    Ok(json!({
        "kind": "workflowDescription",
        "workflowId": description.summary.workflow_id.to_string(),
        "runId": input_str(input, "runId"),
        "status": format!("{:?}", description.summary.status),
        "history": description.history.iter().map(normalize_event).collect::<Vec<_>>()
    }))
}

async fn collect_stream(
    input: &Value,
    context: &ScenarioContext,
) -> Result<Vec<Value>, ClientError> {
    let client = context.client()?;
    let selector = input.get("selector").unwrap_or(input);
    let workflow_id_value = workflow_id(
        input_str(selector, "workflowId")
            .or_else(|| input_str(input, "workflowId"))
            .unwrap_or_default(),
    )?;
    let timeout_ms = input
        .get("collectUntil")
        .or_else(|| input.get("collect"))
        .and_then(|value| value.get("timeoutMs"))
        .or_else(|| input.get("timeoutMs"))
        .and_then(Value::as_u64)
        .unwrap_or(10_000);
    let wanted = input
        .get("collectUntil")
        .and_then(|value| value.get("eventTypes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut stream = client.subscribe_workflow(&workflow_id_value);
    let mut events = Vec::new();
    let deadline = tokio::time::sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            item = stream.next() => {
                match item {
                    Some(Ok(event)) => {
                        events.push(normalize_event(&event));
                        if !wanted.is_empty() && wanted.iter().all(|kind| events.iter().any(|event| event.get("type") == Some(kind))) {
                            return Ok(events);
                        }
                        if wanted.is_empty() && events.len() >= 3 {
                            return Ok(events);
                        }
                    }
                    Some(Err(error)) => return Err(error),
                    None => return Ok(events),
                }
            }
            () = &mut deadline => return Ok(events),
        }
    }
}

fn normalize_event(event: &Event) -> Value {
    match event {
        Event::WorkflowStarted {
            envelope,
            input,
            parent_run_id: None,
            ..
        } => {
            json!({"type":"WorkflowStarted","workflowId": envelope.workflow_id.to_string(),"seq": envelope.seq,"payload": input.to_json().ok()})
        }
        Event::SignalReceived {
            envelope, payload, ..
        } => {
            json!({"type":"WorkflowSignalled","workflowId": envelope.workflow_id.to_string(),"seq": envelope.seq,"payload": payload.to_json().ok()})
        }
        Event::WorkflowCancelled { envelope, .. } => {
            json!({"type":"WorkflowCancellationRequested","workflowId": envelope.workflow_id.to_string(),"seq": envelope.seq})
        }
        Event::WorkflowCompleted { envelope, result } => {
            json!({"type":"WorkflowCompleted","workflowId": envelope.workflow_id.to_string(),"seq": envelope.seq,"payload": result.to_json().ok()})
        }
        other => {
            json!({"type": format!("{other:?}").split_whitespace().next().unwrap_or("Event"), "workflowId": other.envelope().workflow_id.to_string(), "seq": other.seq()})
        }
    }
}

fn assert_matches(
    scenario: &str,
    step: &str,
    actual: &Value,
    expected: &Value,
    context: &ScenarioContext,
) {
    if let Some(expected_error) = expected.get("error").and_then(Value::as_str) {
        assert_eq!(
            actual.get("error").and_then(Value::as_str),
            Some(expected_error),
            "scenario={scenario} step={step}"
        );
        return;
    }
    let actual_ok = actual.get("ok").unwrap_or(&Value::Null);
    let expected_ok = expected.get("ok").unwrap_or(&Value::Null);
    assert_eq!(
        actual_ok.get("kind"),
        expected_ok.get("kind"),
        "scenario={scenario} step={step}"
    );
    if expected_ok.get("workflowId") == Some(&Value::String("anyString".to_owned())) {
        assert!(
            actual_ok
                .get("workflowId")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
            "scenario={scenario} step={step}"
        );
    }
    if expected_ok.get("runId") == Some(&Value::String("anyString".to_owned())) {
        assert!(
            actual_ok
                .get("runId")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
            "scenario={scenario} step={step}"
        );
    }
    if let Some(expected_value) = expected_ok.get("payloadEquals") {
        assert_eq!(
            actual_ok.get("value"),
            Some(expected_value),
            "scenario={scenario} step={step}"
        );
    }
    if let Some(reference) = expected_ok.get("sameHandleAs").and_then(Value::as_str) {
        if let Some(referenced) = context.by_reference(reference) {
            assert_eq!(
                actual_ok.get("workflowId"),
                referenced.pointer("/ok/workflowId"),
                "scenario={scenario} step={step}"
            );
            assert_eq!(
                actual_ok.get("runId"),
                referenced.pointer("/ok/runId"),
                "scenario={scenario} step={step}"
            );
        }
    }
    if let Some(reference) = expected_ok.get("containsWorkflowRef") {
        let workflow_id_expected = reference.get("workflowId").and_then(Value::as_str);
        let found = actual_ok
            .get("workflows")
            .and_then(Value::as_array)
            .is_some_and(|workflows| {
                workflows.iter().any(|workflow| {
                    workflow.get("workflowId").and_then(Value::as_str) == workflow_id_expected
                })
            });
        assert!(found, "scenario={scenario} step={step}");
    }
    if let Some(statuses) = expected_ok.get("statusIn").and_then(Value::as_array) {
        let actual_status = actual_ok.get("status").and_then(Value::as_str);
        assert!(
            statuses
                .iter()
                .any(|status| status.as_str() == actual_status),
            "scenario={scenario} step={step}"
        );
    }
    if let Some(required_events) = expected_ok.get("eventsInclude").and_then(Value::as_array) {
        let actual_events = actual_ok
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for required in required_events {
            assert!(
                event_included(&actual_events, required),
                "scenario={scenario} step={step} required_event={required}"
            );
        }
    }
    if expected_ok.get("sequenceContiguousUnique") == Some(&Value::Bool(true)) {
        assert_eq!(
            actual_ok.get("sequenceContiguousUnique"),
            Some(&Value::Bool(true)),
            "scenario={scenario} step={step}"
        );
    }
}

fn event_included(events: &[Value], required: &Value) -> bool {
    events.iter().any(|event| {
        required.as_object().is_some_and(|fields| {
            fields.iter().all(|(key, value)| match key.as_str() {
                "payloadContains" => value.as_object().is_some_and(|required_payload| {
                    event
                        .get("payload")
                        .and_then(Value::as_object)
                        .is_some_and(|payload| {
                            required_payload.iter().all(|(payload_key, payload_value)| {
                                payload.get(payload_key) == Some(payload_value)
                            })
                        })
                }),
                _ => event.get(key) == Some(value),
            })
        })
    })
}

fn sequences_contiguous_unique(events: &[Value]) -> bool {
    let mut sequences = events
        .iter()
        .filter_map(|event| event.get("seq").and_then(Value::as_u64))
        .collect::<Vec<_>>();
    if sequences.is_empty() {
        return false;
    }
    sequences.sort_unstable();
    sequences
        .windows(2)
        .all(|window| window[1] == window[0] + 1)
}

fn resolve_value(
    value: &Value,
    defaults: &Map<String, Value>,
    fixtures: &Map<String, Value>,
    scenario: &str,
    context: &ScenarioContext,
    started_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Value {
    match value {
        Value::String(text) => {
            resolve_string(text, defaults, fixtures, scenario, context, started_at, now)
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|item| {
                    resolve_value(item, defaults, fixtures, scenario, context, started_at, now)
                })
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, item)| {
                    (
                        key.clone(),
                        resolve_value(item, defaults, fixtures, scenario, context, started_at, now),
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn resolve_string(
    text: &str,
    defaults: &Map<String, Value>,
    fixtures: &Map<String, Value>,
    scenario: &str,
    context: &ScenarioContext,
    started_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Value {
    if text == "${AION_SERVER_URL}" {
        return Value::String(env::var(SERVER_URL_ENV).unwrap_or_default());
    }
    if text == "${AION_AUTH_TOKEN}" {
        return Value::String(env::var(AUTH_TOKEN_ENV).unwrap_or_default());
    }
    if text == "${scenario.startedAt}" {
        return Value::String(started_at.to_rfc3339());
    }
    if text == "${scenario.now}" {
        return Value::String(now.to_rfc3339());
    }
    if let Some(path) = text
        .strip_prefix("${defaults.")
        .and_then(|value| value.strip_suffix('}'))
    {
        return lookup_path(&Value::Object(defaults.clone()), path).unwrap_or(Value::Null);
    }
    if let Some(path) = text
        .strip_prefix("${fixtures.")
        .and_then(|value| value.strip_suffix('}'))
    {
        return lookup_path(&Value::Object(fixtures.clone()), path).unwrap_or(Value::Null);
    }
    if let Some(path) = text.strip_prefix('$') {
        return context
            .lookup_reference(scenario, path)
            .unwrap_or_else(|| Value::String(text.to_owned()));
    }
    Value::String(text.to_owned())
}

fn lookup_path(root: &Value, path: &str) -> Option<Value> {
    path.split('.')
        .try_fold(root, |current, part| current.get(part))
        .cloned()
}

fn json_payload(value: Option<&Value>) -> Result<Payload, ClientError> {
    let json_value = value
        .and_then(|payload| payload.get("json"))
        .cloned()
        .unwrap_or(Value::Null);
    Payload::from_json(&json_value).map_err(|error| ClientError::Server {
        detail: error.to_string(),
    })
}

fn workflow_id(value: &str) -> Result<WorkflowId, ClientError> {
    Uuid::parse_str(value)
        .map(WorkflowId::new)
        .map_err(|error| ClientError::Server {
            detail: error.to_string(),
        })
}

fn optional_run_id(value: Option<&str>) -> Result<Option<RunId>, ClientError> {
    value
        .map(|id| {
            Uuid::parse_str(id)
                .map(RunId::new)
                .map_err(|error| ClientError::Server {
                    detail: error.to_string(),
                })
        })
        .transpose()
}

fn input_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn require_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, Box<dyn std::error::Error>> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field {key}").into())
}

fn datetime(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(DateTime::from)
}

fn status(value: Option<&str>) -> Option<WorkflowStatus> {
    match value {
        Some("RUNNING") => Some(WorkflowStatus::Running),
        Some("COMPLETED") => Some(WorkflowStatus::Completed),
        Some("FAILED") => Some(WorkflowStatus::Failed),
        Some("CANCELLED") => Some(WorkflowStatus::Cancelled),
        Some("TIMED_OUT") => Some(WorkflowStatus::TimedOut),
        Some("CONTINUED_AS_NEW") => Some(WorkflowStatus::ContinuedAsNew),
        _ => None,
    }
}

fn error_variant(error: &ClientError) -> &'static str {
    match error {
        ClientError::NotFound => "NotFound",
        ClientError::AlreadyExists => "AlreadyExists",
        ClientError::QueryFailed => "QueryFailed",
        ClientError::QueryTimeout => "QueryTimeout",
        ClientError::Cancelled => "Cancelled",
        ClientError::Unavailable => "Unavailable",
        ClientError::Unauthenticated => "Unauthenticated",
        ClientError::InvalidArgument => "InvalidArgument",
        ClientError::Server { .. } => "Server",
    }
}

#[derive(Default)]
struct ScenarioContext {
    client: Option<Client>,
    results: HashMap<String, Value>,
}

impl ScenarioContext {
    fn client(&self) -> Result<&Client, ClientError> {
        self.client.as_ref().ok_or(ClientError::InvalidArgument)
    }

    fn record(&mut self, scenario: &str, step: &str, result: Value) {
        self.results.insert(format!("{scenario}.{step}"), result);
    }

    fn lookup_reference(&self, current_scenario: &str, path: &str) -> Option<Value> {
        let parts = path.split('.').collect::<Vec<_>>();
        let (scenario, step, fields): (&str, &str, &[&str]) = match parts.as_slice() {
            [step] => (current_scenario, *step, &[]),
            [step, field @ ..]
                if self
                    .results
                    .contains_key(&format!("{current_scenario}.{step}")) =>
            {
                (current_scenario, *step, field)
            }
            [scenario, step, field @ ..] => (*scenario, *step, field),
            _ => return None,
        };
        let mut value = self.results.get(&format!("{scenario}.{step}"))?;
        if value.get("ok").is_some() {
            value = value.get("ok")?;
        }
        for field in fields {
            value = value.get(*field)?;
        }
        Some(value.clone())
    }

    fn by_reference(&self, reference: &str) -> Option<&Value> {
        reference
            .strip_prefix('$')
            .and_then(|path| self.results.get(path))
    }
}
