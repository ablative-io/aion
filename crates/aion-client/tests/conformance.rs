//! Live conformance harness for the shared aion client scenario contract.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aion_client::transport::ws::EVENT_STREAM_PATH;
use aion_client::{Client, ClientAuth, ClientError, ListPage, StartOptions, TlsOptions};
use aion_core::{ContentType, Event, Payload, RunId, WorkflowFilter, WorkflowId, WorkflowStatus};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde_json::{Map, Value, json};
use uuid::Uuid;

const SCENARIOS_PATH: &str = "conformance/aion-clients/scenarios.json";
const SERVER_URL_ENV: &str = "AION_SERVER_URL";
const STREAM_URL_ENV: &str = "AION_STREAM_URL";
const AUTH_TOKEN_ENV: &str = "AION_AUTH_TOKEN";

/// Caller identity presented to the server's development-header extraction:
/// the conformance credential holds a grant for `conformance` only — never
/// `conformance-denied` — which is exactly the grant shape the
/// namespace-denied and not-found-anti-leak scenarios pin.
const HARNESS_SUBJECT: &str = "conformance-harness";
const HARNESS_NAMESPACES: [&str; 1] = ["conformance"];

/// The harness attaches every stream from the workflow's beginning
/// (`resume_from_seq = 1`, the wire's documented full-history replay): the
/// scenarios assert on events recorded before the subscribe step ran
/// (`WorkflowStarted`), which a live-tail attach can never deliver. This
/// mirrors the TypeScript harness's initial-attach mapping. Reconnect
/// cursors (`last delivered + 1`) are the SDK's own resume loop, untouched.
const HARNESS_ATTACH_FROM: NonZeroU64 = NonZeroU64::MIN;

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
    let mut context = ScenarioContext::default();
    let result = run_scenario_steps(
        scenario_id,
        steps,
        defaults,
        fixtures,
        server_url,
        &mut context,
    )
    .await;
    context.teardown();
    result
}

async fn run_scenario_steps(
    scenario_id: &str,
    steps: &[Value],
    defaults: &Map<String, Value>,
    fixtures: &Map<String, Value>,
    server_url: &str,
    context: &mut ScenarioContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let started_at = Utc::now() - chrono::Duration::seconds(30);
    let now = Utc::now() + chrono::Duration::seconds(30);
    for step in steps {
        let step_id = require_str(step, "id")?;
        let operation = require_str(step, "operation")?;
        let input = resolve_value(
            step.get("input").unwrap_or(&Value::Null),
            defaults,
            fixtures,
            scenario_id,
            context,
            started_at,
            now,
        );
        let expected = resolve_value(
            step.get("expect").ok_or("step expect is required")?,
            defaults,
            fixtures,
            scenario_id,
            context,
            started_at,
            now,
        );
        let actual = execute_step(operation, &input, context, server_url).await;
        let (normalized, error_identity) = match actual {
            Ok(value) => (json!({ "ok": value }), None),
            Err(error) => (
                json!({ "error": error_variant(&error) }),
                Some(json!({
                    "code": error_variant(&error),
                    "message": error.to_string()
                })),
            ),
        };
        println!(
            "AION_CONFORMANCE sdk=rust scenario={scenario_id} step={step_id} result={}",
            serde_json::to_string(&normalized)?
        );
        assert_matches(
            scenario_id,
            step_id,
            &normalized,
            &expected,
            context,
            error_identity.as_ref(),
        );
        context.record(scenario_id, step_id, normalized);
        if let Some(identity) = error_identity {
            context.record_error_identity(scenario_id, step_id, identity);
        }
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
                execute_subscribe_collect(input, context)
            } else {
                let events = collect_stream(input, context).await?;
                Ok(json!({ "kind": "eventStream", "events": events }))
            }
        }
        "harness.forceDisconnect" => execute_force_disconnect(context).await,
        "harness.assertStream" => execute_assert_stream(input, context).await,
        other => Err(ClientError::server(format!(
            "unsupported conformance operation {other}"
        ))),
    }
}

async fn execute_connect(
    input: &Value,
    context: &mut ScenarioContext,
    server_url: &str,
) -> Result<Value, ClientError> {
    let mut builder = Client::builder(server_url)
        .with_namespace(input_str(input, "namespace").unwrap_or("conformance"))
        .with_subject(HARNESS_SUBJECT)
        .with_authorized_namespaces(HARNESS_NAMESPACES);
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
    // Event subscriptions ride the server's HTTP/WebSocket listener — a
    // SEPARATE address from the gRPC endpoint — named explicitly by
    // AION_STREAM_URL (the same convention as the Python harness; nothing is
    // derived from the gRPC URL). For plaintext ws the stream is pointed
    // through a local TCP relay so `harness.forceDisconnect` can sever live
    // stream sockets — and only stream sockets — without touching the
    // server; the relay is a raw byte pipe, so wss endpoints connect
    // directly and forced disconnects are refused with a precise error.
    context.close_relay();
    if let Ok(stream_url) = env::var(STREAM_URL_ENV) {
        if !stream_url.is_empty() {
            let endpoint = ws_stream_endpoint(&stream_url)?;
            if let Some(authority) = endpoint
                .strip_prefix("ws://")
                .and_then(|rest| rest.split('/').next())
            {
                let (host, port) = match authority.split_once(':') {
                    Some((host, port)) => (
                        host.to_owned(),
                        port.parse::<u16>().map_err(|_| {
                            ClientError::server(format!(
                                "{STREAM_URL_ENV} port {port} is not a u16"
                            ))
                        })?,
                    ),
                    None => (authority.to_owned(), 80),
                };
                let relay = TcpRelay::start(host, port).await?;
                builder = builder.with_stream_endpoint(format!(
                    "ws://127.0.0.1:{}{EVENT_STREAM_PATH}",
                    relay.port
                ));
                context.relay = Some(relay);
            } else {
                // wss endpoints cannot be byte-relayed; connect directly.
                builder = builder.with_stream_endpoint(endpoint);
            }
        }
    }
    let client = builder.build().await?;
    context.client = Some(client);
    Ok(json!({ "kind": "client" }))
}

/// Maps the harness `AION_STREAM_URL` onto the ws(s) `/events/stream` URL:
/// `http(s)` schemes are protocol-mapped (the same listener serves the
/// route), `ws(s)` pass through, and the route path is appended when the URL
/// names only the listener.
fn ws_stream_endpoint(stream_url: &str) -> Result<String, ClientError> {
    let (scheme, rest) = stream_url.split_once("://").ok_or_else(|| {
        ClientError::server(format!(
            "{STREAM_URL_ENV} must be an absolute http(s):// or ws(s):// URL, got {stream_url}"
        ))
    })?;
    let scheme = match scheme {
        "http" | "ws" => "ws",
        "https" | "wss" => "wss",
        other => {
            return Err(ClientError::server(format!(
                "{STREAM_URL_ENV} scheme {other}:// is not http(s) or ws(s)"
            )));
        }
    };
    let rest = rest.trim_end_matches('/');
    if rest.ends_with("/events/stream") {
        Ok(format!("{scheme}://{rest}"))
    } else {
        Ok(format!("{scheme}://{rest}{EVENT_STREAM_PATH}"))
    }
}

/// The disconnect-resume choreography's subscribe step: start the SINGLE
/// background collector every later harness step (`forceDisconnect`,
/// `assertStream`) observes — one stream object spanning the forced
/// disconnect, never a fresh subscription.
fn execute_subscribe_collect(
    input: &Value,
    context: &mut ScenarioContext,
) -> Result<Value, ClientError> {
    let client = context.client()?;
    let selector = input.get("selector").unwrap_or(input);
    let workflow = workflow_id(
        input_str(selector, "workflowId")
            .or_else(|| input_str(input, "workflowId"))
            .unwrap_or_default(),
    )?;
    let stream = client.subscribe_workflow_from(&workflow, HARNESS_ATTACH_FROM);
    context.collector = Some(StreamCollector::start(stream));
    context.collect_plan = Some(CollectPlan::from_input(input)?);
    Ok(json!({ "kind": "eventStreamStarted" }))
}

/// Severs the live piped stream sockets once the before-disconnect minimum
/// has been observed; the relay listener stays up for the SDK's reconnect.
async fn execute_force_disconnect(context: &mut ScenarioContext) -> Result<Value, ClientError> {
    let plan = context.collect_plan.clone().ok_or_else(|| {
        ClientError::server("harness.forceDisconnect requires a prior subscribe step with collect")
    })?;
    let collector = context.collector.as_mut().ok_or_else(|| {
        ClientError::server("harness.forceDisconnect requires a running stream collector")
    })?;
    let reached = collector
        .wait_for_total(plan.minimum_events_before_disconnect, plan.timeout)
        .await?;
    if !reached {
        return Err(ClientError::server(format!(
            "stream delivered {} event(s) within {:?}; needed {} before the forced disconnect",
            collector.len(),
            plan.timeout,
            plan.minimum_events_before_disconnect
        )));
    }
    collector.mark_disconnect();
    let relay = context.relay.as_ref().ok_or_else(|| {
        ClientError::server(
            "harness.forceDisconnect requires the plaintext ws:// stream relay; \
             wss stream endpoints cannot be byte-relayed",
        )
    })?;
    let dropped = relay.force_disconnect();
    if dropped == 0 {
        return Err(ClientError::server(
            "no live stream connection was piped through the relay to disconnect",
        ));
    }
    Ok(json!({ "kind": "disconnectInjected", "droppedConnections": dropped }))
}

/// Awaits the subscribe step's collector and reports its single accumulated
/// list (spanning the forced disconnect) for the step's assertions. A
/// timeout returns the partial list so the expectations fail with evidence
/// instead of the harness hanging.
async fn execute_assert_stream(
    input: &Value,
    context: &mut ScenarioContext,
) -> Result<Value, ClientError> {
    let plan = context.collect_plan.clone().ok_or_else(|| {
        ClientError::server("harness.assertStream requires a prior subscribe step with collect")
    })?;
    let collector = context.collector.as_mut().ok_or_else(|| {
        ClientError::server("harness.assertStream requires a running stream collector")
    })?;
    let mut target = plan.minimum_events_before_disconnect + plan.minimum_events_after_reconnect;
    if let Some(mark) = collector.disconnect_mark {
        target = target.max(mark + plan.minimum_events_after_reconnect);
    }
    let timeout = input
        .get("timeoutMs")
        .and_then(Value::as_u64)
        .map_or(plan.timeout, Duration::from_millis);
    let reached_target = collector.wait_for_total(target, timeout).await?;
    if !reached_target {
        // Fall through to the partial list so the step's expectations fail
        // with the accumulated evidence instead of the harness hanging.
        println!(
            "AION_CONFORMANCE sdk=rust note=assertStream timed out short of {target} event(s); \
             asserting over the partial list"
        );
    }
    let events = collector.events();
    Ok(json!({
        "kind": "eventStream",
        "events": events,
        "sequenceContiguousUnique": sequences_contiguous_unique(&events),
    }))
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
                routing_key: input_str(input, "routingKey").map(ToOwned::to_owned),
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

/// Deadline for the fixture workflow to finish registering its query
/// handlers: registration is workflow code racing the caller after `start`
/// returns, so the harness retries `UnknownQuery` outcomes within this
/// window (the same fixture-readiness convention as the committed server
/// e2e matrix in `crates/aion-server/tests/query_workflow.rs`).
const QUERY_REGISTRATION_DEADLINE: Duration = Duration::from_secs(10);

async fn execute_query(input: &Value, context: &mut ScenarioContext) -> Result<Value, ClientError> {
    let client = context.client()?;
    let wf_id = workflow_id(input_str(input, "workflowId").unwrap_or_default())?;
    let run_id = optional_run_id(input_str(input, "runId"))?;
    let deadline_ms = input
        .get("deadlineMs")
        .and_then(Value::as_u64)
        .unwrap_or(5000);
    let registration_deadline = tokio::time::Instant::now() + QUERY_REGISTRATION_DEADLINE;
    let payload = loop {
        let attempt = client
            .query(
                &wf_id,
                run_id.as_ref(),
                input_str(input, "queryName").unwrap_or_default(),
                Payload::new(ContentType::Json, Vec::new()),
                Duration::from_millis(deadline_ms),
            )
            .await;
        match attempt {
            Err(ClientError::UnknownQuery { .. })
                if tokio::time::Instant::now() < registration_deadline =>
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            other => break other?,
        }
    };
    let decoded = payload
        .to_json()
        .map_err(|error| ClientError::server(error.to_string()))?;
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
                "status": status_name(summary.status)
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
        "status": status_name(description.summary.status),
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
    // Full-history attach: the expected events were recorded before this
    // subscribe step ran (see HARNESS_ATTACH_FROM).
    let mut stream = client.subscribe_workflow_from(&workflow_id_value, HARNESS_ATTACH_FROM);
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
            run_id: _,
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
    error_identity: Option<&Value>,
) {
    if let Some(expected_error) = expected.get("error").and_then(Value::as_str) {
        assert_eq!(
            actual.get("error").and_then(Value::as_str),
            Some(expected_error),
            "scenario={scenario} step={step}"
        );
        assert_error_identity_matches(
            scenario,
            step,
            expected.get("errorSameAs").and_then(Value::as_str),
            context,
            error_identity,
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
    for identifier in ["workflowId", "runId"] {
        if expected_ok.get(identifier) == Some(&Value::String("anyString".to_owned())) {
            assert!(
                actual_ok
                    .get(identifier)
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty()),
                "scenario={scenario} step={step} field={identifier}"
            );
        }
    }
    if let Some(expected_value) = expected_ok.get("payloadEquals") {
        assert_eq!(
            actual_ok.get("value"),
            Some(expected_value),
            "scenario={scenario} step={step}"
        );
    }
    if let Some(reference) = expected_ok.get("sameHandleAs") {
        assert_same_handle(scenario, step, actual_ok, reference, context);
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

/// Pins a `sameHandleAs` expectation: the step's handle identity equals the
/// referenced step's. The resolver may already have replaced the
/// `$scenario.step` reference with the recorded step's ok payload; both
/// shapes pin the same handle identity, and a dangling reference is a hard
/// failure.
fn assert_same_handle(
    scenario: &str,
    step: &str,
    actual_ok: &Value,
    reference: &Value,
    context: &ScenarioContext,
) {
    let referenced_ok = match reference {
        Value::String(reference) => context
            .by_reference(reference)
            .and_then(|referenced| referenced.get("ok"))
            .cloned(),
        Value::Object(_) => Some(reference.clone()),
        _ => None,
    };
    assert!(
        referenced_ok.is_some(),
        "scenario={scenario} step={step} sameHandleAs reference is unresolvable"
    );
    let referenced_ok = referenced_ok.unwrap_or(Value::Null);
    assert_eq!(
        actual_ok.get("workflowId"),
        referenced_ok.get("workflowId"),
        "scenario={scenario} step={step}"
    );
    assert_eq!(
        actual_ok.get("runId"),
        referenced_ok.get("runId"),
        "scenario={scenario} step={step}"
    );
}

/// Asserts the `errorSameAs` expectation: the current step's SDK-observable
/// error identity must be byte-identical to the identity recorded by the
/// referenced step, pinning the anti-existence-leak equivalence.
fn assert_error_identity_matches(
    scenario: &str,
    step: &str,
    reference: Option<&str>,
    context: &ScenarioContext,
    error_identity: Option<&Value>,
) {
    let Some(reference) = reference else {
        return;
    };
    let recorded = context.error_identity(reference);
    assert!(
        recorded.is_some(),
        "scenario={scenario} step={step} errorSameAs references unrecorded step {reference}"
    );
    assert!(
        error_identity.is_some(),
        "scenario={scenario} step={step} errorSameAs requires the step to surface an error"
    );
    assert_eq!(
        error_identity, recorded,
        "scenario={scenario} step={step} errorSameAs={reference}"
    );
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
    Payload::from_json(&json_value).map_err(|error| ClientError::server(error.to_string()))
}

fn workflow_id(value: &str) -> Result<WorkflowId, ClientError> {
    Uuid::parse_str(value)
        .map(WorkflowId::new)
        .map_err(|error| ClientError::server(error.to_string()))
}

fn optional_run_id(value: Option<&str>) -> Result<Option<RunId>, ClientError> {
    value
        .map(|id| {
            Uuid::parse_str(id)
                .map(RunId::new)
                .map_err(|error| ClientError::server(error.to_string()))
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

/// Renders a workflow status in the scenario document's language-neutral
/// spelling (`RUNNING`, `TIMED_OUT`, ...).
fn status_name(status: WorkflowStatus) -> &'static str {
    match status {
        WorkflowStatus::Running => "RUNNING",
        WorkflowStatus::Completed => "COMPLETED",
        WorkflowStatus::Failed => "FAILED",
        WorkflowStatus::Cancelled => "CANCELLED",
        WorkflowStatus::TimedOut => "TIMED_OUT",
        WorkflowStatus::ContinuedAsNew => "CONTINUED_AS_NEW",
    }
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
        ClientError::NotFound { .. } => "NotFound",
        ClientError::AlreadyExists { .. } => "AlreadyExists",
        ClientError::QueryFailed { .. } => "QueryFailed",
        ClientError::QueryTimeout { .. } => "QueryTimeout",
        ClientError::UnknownQuery { .. } => "UnknownQuery",
        ClientError::NotRunning { .. } => "NotRunning",
        ClientError::Cancelled { .. } => "Cancelled",
        ClientError::Unavailable { .. } => "Unavailable",
        ClientError::Unauthenticated { .. } => "Unauthenticated",
        ClientError::NamespaceDenied { .. } => "NamespaceDenied",
        ClientError::InvalidArgument { .. } => "InvalidArgument",
        ClientError::Server { .. } => "Server",
    }
}

/// Per-scenario harness plan parsed from the subscribe step's `collect` block.
#[derive(Clone, Debug)]
struct CollectPlan {
    minimum_events_before_disconnect: usize,
    minimum_events_after_reconnect: usize,
    timeout: Duration,
}

impl CollectPlan {
    fn from_input(input: &Value) -> Result<Self, ClientError> {
        let collect = input
            .get("collect")
            .and_then(Value::as_object)
            .ok_or_else(|| ClientError::server("subscribe collect input must be a JSON object"))?;
        let count = |key: &str| -> Result<usize, ClientError> {
            match collect.get(key) {
                None => Ok(0),
                Some(value) => value
                    .as_u64()
                    .and_then(|count| usize::try_from(count).ok())
                    .ok_or_else(|| {
                        ClientError::server(format!(
                            "subscribe collect input {key} must be a non-negative integer"
                        ))
                    }),
            }
        };
        let timeout_ms = collect
            .get("timeoutMs")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                ClientError::server(
                    "subscribe collect input requires a positive timeoutMs bounding the \
                     harness waits",
                )
            })?;
        Ok(Self {
            minimum_events_before_disconnect: count("minimumEventsBeforeDisconnect")?,
            minimum_events_after_reconnect: count("minimumEventsAfterReconnect")?,
            timeout: Duration::from_millis(timeout_ms),
        })
    }
}

/// Transparent local TCP relay in front of the server's WebSocket listener.
/// The SDK's stream endpoint points at it, so aborting the currently piped
/// link tasks injects a real transient disconnect while the listener keeps
/// accepting the resume reconnect.
struct TcpRelay {
    port: u16,
    links: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl TcpRelay {
    async fn start(target_host: String, target_port: u16) -> Result<Self, ClientError> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|error| ClientError::server(format!("relay bind failed: {error}")))?;
        let port = listener
            .local_addr()
            .map_err(|error| ClientError::server(format!("relay local_addr failed: {error}")))?
            .port();
        let links: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> = Arc::default();
        let accept_links = Arc::clone(&links);
        let accept_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = listener.accept().await else {
                    return;
                };
                let target_host = target_host.clone();
                let link = tokio::spawn(async move {
                    let Ok(mut upstream) =
                        tokio::net::TcpStream::connect((target_host.as_str(), target_port)).await
                    else {
                        // Dropping the downstream socket IS the report: the
                        // SDK observes the failed stream and surfaces it.
                        return;
                    };
                    let copied =
                        tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
                    // A reset peer ends the pipe; both sockets drop here,
                    // which is exactly the forced-disconnect outcome.
                    drop(copied);
                });
                match accept_links.lock() {
                    Ok(mut links) => {
                        links.retain(|handle| !handle.is_finished());
                        links.push(link);
                    }
                    Err(_poisoned) => {
                        // The harness thread that poisoned the lock already
                        // failed the test; stop accepting.
                        link.abort();
                        return;
                    }
                }
            }
        });
        Ok(Self {
            port,
            links,
            accept_task,
        })
    }

    /// Aborts every live piped link (dropping both sockets); the listener
    /// stays up so the reconnect succeeds through the same relay endpoint.
    /// Returns how many live links were severed.
    fn force_disconnect(&self) -> usize {
        let Ok(mut links) = self.links.lock() else {
            return 0;
        };
        let mut severed = 0;
        for link in links.drain(..) {
            if !link.is_finished() {
                severed += 1;
            }
            link.abort();
        }
        severed
    }

    fn close(&self) {
        self.accept_task.abort();
        // Any count is fine at teardown; live links are severed either way.
        self.force_disconnect();
    }
}

/// The single background consumer of the disconnect-resume subscribe step's
/// stream. `assertStream` asserts over THIS collector's accumulated list —
/// one stream spanning the forced disconnect, never a new subscription.
struct StreamCollector {
    events: Arc<Mutex<Vec<Value>>>,
    failure: Arc<Mutex<Option<ClientError>>>,
    task: tokio::task::JoinHandle<()>,
    disconnect_mark: Option<usize>,
}

impl StreamCollector {
    fn start(mut stream: aion_client::stream::EventStream) -> Self {
        let events: Arc<Mutex<Vec<Value>>> = Arc::default();
        let failure: Arc<Mutex<Option<ClientError>>> = Arc::default();
        let sink = Arc::clone(&events);
        let failure_sink = Arc::clone(&failure);
        let task = tokio::spawn(async move {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(event) => {
                        if let Ok(mut events) = sink.lock() {
                            events.push(normalize_event(&event));
                        }
                    }
                    Err(error) => {
                        if let Ok(mut failure) = failure_sink.lock() {
                            *failure = Some(error);
                        }
                        return;
                    }
                }
            }
        });
        Self {
            events,
            failure,
            task,
            disconnect_mark: None,
        }
    }

    fn events(&self) -> Vec<Value> {
        self.events
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default()
    }

    fn len(&self) -> usize {
        self.events.lock().map(|events| events.len()).unwrap_or(0)
    }

    fn failure(&self) -> Option<ClientError> {
        self.failure.lock().ok().and_then(|failure| failure.clone())
    }

    /// Snapshots how many events were delivered before the forced drop.
    fn mark_disconnect(&mut self) {
        self.disconnect_mark = Some(self.len());
    }

    /// Waits until `target` events have accumulated. Returns `Ok(false)` on
    /// timeout or stream end short of the target; a terminal stream failure
    /// is raised because the target can never be reached.
    async fn wait_for_total(&self, target: usize, timeout: Duration) -> Result<bool, ClientError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(failure) = self.failure() {
                return Err(failure);
            }
            if self.len() >= target {
                return Ok(true);
            }
            if self.task.is_finished() || tokio::time::Instant::now() >= deadline {
                return Ok(self.len() >= target);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn stop(&self) {
        self.task.abort();
    }
}

#[derive(Default)]
struct ScenarioContext {
    client: Option<Client>,
    relay: Option<TcpRelay>,
    collector: Option<StreamCollector>,
    collect_plan: Option<CollectPlan>,
    results: HashMap<String, Value>,
    error_identities: HashMap<String, Value>,
}

impl ScenarioContext {
    fn client(&self) -> Result<&Client, ClientError> {
        self.client
            .as_ref()
            .ok_or_else(|| ClientError::server("scenario has not connected yet"))
    }

    fn close_relay(&mut self) {
        if let Some(relay) = self.relay.take() {
            relay.close();
        }
    }

    fn teardown(&mut self) {
        if let Some(collector) = self.collector.take() {
            collector.stop();
        }
        self.close_relay();
    }

    fn record(&mut self, scenario: &str, step: &str, result: Value) {
        self.results.insert(format!("{scenario}.{step}"), result);
    }

    fn record_error_identity(&mut self, scenario: &str, step: &str, identity: Value) {
        self.error_identities
            .insert(format!("{scenario}.{step}"), identity);
    }

    fn error_identity(&self, reference: &str) -> Option<&Value> {
        self.error_identities.get(reference)
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
