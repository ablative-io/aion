//! REAL end-to-end test: spawn the actual `norn` binary through [`NornHarness`] (the
//! [`AgentHarness`] trait) and drive one full activity attempt against a LOCAL mock
//! OpenAI-compatible provider — no network, no credentials.
//!
//! It asserts the four things NOI-4 must prove against the real process boundary:
//!
//! 1. **initialize capabilities** — the parsed [`InterventionCapabilities`] advertise
//!    `inject_message` + `cancel`,
//! 2. **live `ActivityEvent`s arrive** — real `event/*` notifications from the run stream through
//!    [`AgentSession::events`] and translate into neutral events,
//! 3. **an intervene reaches the child and is acked** — a neutral [`InterventionKind::InjectMessage`]
//!    maps to `intervene/injectMessage`, reaches the running Norn agent, and its ack resolves, and
//! 4. **`wait_result` returns the [`Payload`]** — the id-matched `run/execute` Response carries the
//!    versioned stop envelope (`envelope_version: 1`), and the completed run's `output` VALUE (a
//!    JSON string for this schema-less run) is captured as the terminal activity output.
//!
//! The handshake also gates on the `initialize` result's `protocol: "norn-driven/1"` field, so a
//! stale `norn` binary fails `harness.start` with a protocol error rather than running.
//!
//! It is `#[ignore]`d because it spawns the real `norn` binary (slow), consistent with the repo's
//! other slow e2e tests. Run it explicitly:
//!
//! ```text
//! NORN_BIN=/path/to/norn cargo test -p aion-integration-norn --test norn_e2e -- --ignored
//! ```
//!
//! If `NORN_BIN` is unset the test tries `norn` on `PATH`; if neither resolves it fails with a
//! clear message rather than silently passing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use aion_integration_norn::NornHarness;
use aion_integrations::{
    ActivityEventKind, ActivityId, AgentHarness, AgentRunSpec, AgentSession, ContentType,
    InjectPriority, InterventionCommand, InterventionKind, InterventionPrimitive, Payload,
    WorkflowId,
};
use chrono::Utc;
use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The env var name the mock provider's API key lives under (norn reads it via `-c api_key_env`).
const KEY_ENV: &str = "AION_NORN_E2E_KEY";

/// A canned OpenAI-compatible SSE body: one text delta, then a `stop` finish with usage.
const SSE_BODY: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello from the mock provider\"},\"finish_reason\":null}]}\n\n\
     data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":6}}\n\n\
     data: [DONE]\n\n";

/// Resolve the `norn` binary: `NORN_BIN` if set, else `norn` on `PATH`.
fn norn_binary() -> String {
    std::env::var("NORN_BIN").unwrap_or_else(|_| "norn".to_owned())
}

/// A single-connection mock OpenAI-compatible server on 127.0.0.1: it accepts one HTTP request to
/// `/v1/chat/completions` and replies with the canned SSE stream. Returns the bound base URL.
///
/// Hand-rolled over raw TCP so the crate needs no HTTP-server dev-dependency: it reads the request
/// headers until the blank line, then writes a fixed `text/event-stream` response. One request is
/// enough for a single-turn run.
async fn spawn_mock_provider() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
    let addr = listener.local_addr().expect("mock addr");
    tokio::spawn(async move {
        // Serve requests until the test drops (a single-turn run makes exactly one).
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(serve_one(stream));
        }
    });
    format!("http://{addr}")
}

/// Serve one HTTP request: drain the request head, then stream the canned SSE body.
async fn serve_one(mut stream: TcpStream) {
    // Read the request head (up to the blank line) — enough to consume it before responding.
    let mut buf = [0u8; 4096];
    let _ = stream.read(&mut buf).await;
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        SSE_BODY.len(),
        SSE_BODY,
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
}

/// Build the harness pointed at the mock provider.
///
/// The API key value is irrelevant to the mock (it ignores auth); it only needs to exist under
/// `KEY_ENV` so norn's provider construction succeeds. It is set on the CHILD via `with_env`, never
/// on this process's environment.
fn harness(base_url: &str) -> NornHarness {
    NornHarness::with_binary(norn_binary())
        .with_arg("--provider")
        .with_arg("openai-compatible")
        .with_arg("--model")
        .with_arg("mock-model")
        .with_arg("-c")
        .with_arg(format!("base_url={base_url}"))
        .with_arg("-c")
        .with_arg(format!("api_key_env={KEY_ENV}"))
        // No retries: the single-turn mock answers on the first request, so a retry only masks a
        // real failure behind a long backoff.
        .with_arg("-c")
        .with_arg("max_retries=0")
        .with_env(KEY_ENV, "mock-key-unused")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "slow: spawns the real norn binary and a local mock provider"]
async fn norn_harness_drives_a_real_run_end_to_end() {
    let base_url = spawn_mock_provider().await;
    let harness = harness(&base_url);

    let spec = AgentRunSpec::new(
        WorkflowId::new_v4(),
        ActivityId::from_sequence_position(1),
        1,
        "norn-e2e",
        Payload::new(ContentType::Json, b"say hello".to_vec()),
    );

    let mut session = match harness.start(spec).await {
        Ok(session) => session,
        Err(error) => panic!(
            "failed to start the norn harness (is `{}` on PATH or NORN_BIN set?): {error}",
            norn_binary()
        ),
    };

    // (1) initialize capabilities were parsed from the real handshake.
    assert!(
        session
            .capabilities()
            .supports_primitive(InterventionPrimitive::InjectMessage),
        "norn advertises inject_message"
    );
    assert!(
        session
            .capabilities()
            .supports_primitive(InterventionPrimitive::Cancel),
        "norn advertises cancel"
    );

    // (3) an intervene reaches the child and is acked — issued EARLY, while the run is in flight so
    //     the child's inbound intervene reader is still live. A `Normal` injection never pre-empts
    //     the run, so it does not change the deterministic mock result; the ack alone proves the
    //     neutral command reached the real child and round-tripped.
    let command = InterventionCommand {
        workflow_id: WorkflowId::new_v4(),
        activity_id: ActivityId::from_sequence_position(1),
        attempt: 1,
        issued_by: Some("operator".to_owned()),
        issued_at: Utc::now(),
        kind: InterventionKind::InjectMessage {
            text: "noted".to_owned(),
            priority: InjectPriority::Normal,
        },
    };
    let intervene = tokio::time::timeout(Duration::from_secs(10), session.intervene(command)).await;
    assert!(
        matches!(intervene, Ok(Ok(()))),
        "the intervene must reach the child and be acked, got {intervene:?}"
    );

    // (2) live ActivityEvents arrive from the real run. Collect until the terminal Stop event —
    //     NOT until the stream *ends*: the stream ends only when Norn's stdout closes, which does
    //     not happen until the process exits, which requires the terminal result to be read (below)
    //     and the session to drop. Breaking on Stop is the live-stream assertion.
    let mut events = session.events();
    let mut saw_event = false;
    let mut saw_stop = false;
    let collect = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(event) = events.next().await {
            saw_event = true;
            if matches!(event.kind, ActivityEventKind::Stop { .. }) {
                saw_stop = true;
                break;
            }
        }
    })
    .await;
    assert!(
        collect.is_ok(),
        "a terminal Stop event must arrive within the timeout"
    );
    assert!(saw_event, "at least one live ActivityEvent must arrive");
    assert!(saw_stop, "the run must emit a terminal Stop event");
    // Drop the events stream so the pump stops forwarding; the terminal result is read next.
    drop(events);

    // (4) wait_result returns the Payload — the id-matched run/execute Response's stop envelope,
    //     interpreted by the adapter: a completed schema-less run yields the `output` VALUE, a
    //     JSON string carrying the agent's final text (here the mock provider's canned reply).
    let payload = tokio::time::timeout(Duration::from_secs(30), session.wait_result())
        .await
        .expect("wait_result must resolve within the timeout")
        .expect("wait_result returns the terminal payload");
    assert_eq!(payload.content_type(), &ContentType::Json);
    let decoded = payload.to_json().expect("the result payload is JSON");
    let text = decoded.as_str().unwrap_or_else(|| {
        panic!("a schema-less completed run's output is a JSON string, got {decoded}")
    });
    assert!(
        text.contains("hello from the mock provider"),
        "the output carries the agent's final text: {text}"
    );
}
