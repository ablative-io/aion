//! Real-AI liminal PUSH fan-out worker for the durable-agent failover demo.
//!
//! This is the AI counterpart of `spike/liminal-fan-worker`: same liminal
//! server-push transport, same in-band self-registration for the pool
//! `(default, default)`, same redial-to-survivor on owner `kill -9`, and it
//! serves the SAME `collect_four` `fan:N` activities — so the existing
//! `aion_outbox_fixture` workflow, package, and `demo-failover.sh` kill harness
//! drive it verbatim. The only difference is the activity body: instead of
//! returning a canned per-ordinal string, each `fan:N` runs a REAL Norn AI
//! agent step (`norn --print`) and returns the model's answer.
//!
//! That turns the proven exactly-once cross-node failover demo into a tangible
//! "real AI work fans out across the cluster and survives a node kill" demo.
//!
//! Auth: Norn is invoked with `OPENAI_API_KEY` REMOVED from its environment so
//! it uses the operator's ChatGPT OAuth login (the project does not use API
//! keys). A stray key in the ambient environment would otherwise take
//! precedence and fail.
//!
//! Beyond the typed `fan:N` handlers, this worker is the reference "proper
//! worker" wiring: it composes the first-party [`NornHarness`] at its binary
//! root (mirroring `crates/aion-cli/src/harness.rs`) and threads the erased
//! [`AgentHarnessConfig`] through `serve_with_redial`, so a dispatch of the
//! neutral `agent` activity type drives a live, observable, intervenable Norn
//! session through the neutral `AgentHarness` trait. The `fan:N` handlers stay
//! on the plain registry path (single short turns; no per-step event stream).
//!
//! Usage:
//!   norn-fan-worker --address 127.0.0.1:PORT [--address 127.0.0.1:PORT2 ...]
//!                   [--identity <id>] [--ready-file <path>] [--norn-bin <path>]

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use aion_integration_norn::NornHarness;
use aion_integrations::{DynAgentHarness, InterventionCapabilities, InterventionPrimitive};
use aion_worker::{
    ActivityContext, ActivityFailure, ActivityRegistry, AgentHarnessConfig, HandlerFuture,
    RedialTiming, WorkerConfig,
};
use serde_json::Value;

/// The fan-out arity of the `collect_four` fixture.
const FAN_OUT: usize = 4;

/// The activity types `collect_four` dispatches, one per fan-out ordinal, paired
/// with the prompt each fans to a Norn agent. Distinct prompts make the live
/// outputs visibly different (so it reads as real AI, not an echo), while each
/// stays a single short turn that needs no filesystem or tools.
const FAN_TASKS: [(&str, &str); FAN_OUT] = [
    (
        "fan:0",
        "In one vivid sentence, explain exactly-once delivery to a skeptical engineer.",
    ),
    (
        "fan:1",
        "In one vivid sentence, explain how shard failover keeps a distributed system available.",
    ),
    (
        "fan:2",
        "In one vivid sentence, explain what content-addressed storage is and why it matters.",
    ),
    (
        "fan:3",
        "In one vivid sentence, explain why the actor model is a natural fit for durable agents.",
    ),
];

/// Lower bound on the reconnect backoff between candidate dials.
const REDIAL_INITIAL_BACKOFF: Duration = Duration::from_millis(50);
/// Upper bound on the reconnect backoff (a survivor may take a moment to adopt
/// the shard and bring its listener up).
const REDIAL_MAX_BACKOFF: Duration = Duration::from_millis(500);

/// Wall-clock ceiling for a single Norn step, so a wedged step cannot pin an
/// activity slot forever; on elapse we surface a retryable failure and the
/// engine re-dispatches.
const NORN_STEP_TIMEOUT: Duration = Duration::from_secs(180);

/// The neutral activity-type name routed through the composed agent harness
/// rather than the typed registry (the same name the production serve-wiring
/// gate `noi5b_noi6_live_agent_e2e.rs` registers for).
const AGENT_ACTIVITY_TYPE: &str = "agent";

/// The fixture passes each member the JSON string `"in"`, so the handler decodes
/// a [`String`] (matching `liminal-fan-worker`'s `FanInput`). The prompt comes
/// from [`FAN_TASKS`], not the input, so the workflow contract is unchanged.
type FanInput = String;

/// Run one Norn agent step and return its answer.
///
/// `session_id` is stable per ordinal (`<identity>-fan-N`) so a re-dispatch of
/// the same ordinal after a failover RESUMES the same Norn session via
/// `--resume-if-exists` rather than starting over. The four ordinals use four
/// distinct sessions, so concurrent fan-out members never share a session.
async fn run_norn_step(
    norn_bin: String,
    session_id: String,
    prompt: String,
) -> Result<String, ActivityFailure> {
    let invoke = tokio::task::spawn_blocking(move || {
        std::process::Command::new(&norn_bin)
            .arg("--print")
            .arg("--output-format")
            .arg("json")
            .arg("--session-id")
            .arg(&session_id)
            .arg("--resume-if-exists")
            .arg(&prompt)
            // Force the ChatGPT OAuth login: the project does not use API keys,
            // and a stray ambient key would take precedence and fail.
            .env_remove("OPENAI_API_KEY")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
    });

    let output = match tokio::time::timeout(NORN_STEP_TIMEOUT, invoke).await {
        Ok(Ok(Ok(output))) => output,
        Ok(Ok(Err(error))) => {
            return Err(ActivityFailure::retryable(format!(
                "failed to spawn norn: {error}"
            )));
        }
        Ok(Err(join_error)) => {
            return Err(ActivityFailure::retryable(format!(
                "norn task join error: {join_error}"
            )));
        }
        Err(_elapsed) => {
            return Err(ActivityFailure::retryable(format!(
                "norn step exceeded {}s",
                NORN_STEP_TIMEOUT.as_secs()
            )));
        }
    };

    if !output.status.success() {
        return Err(ActivityFailure::retryable(format!(
            "norn exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let envelope: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        ActivityFailure::retryable(format!("norn output was not JSON: {error}"))
    })?;

    // The envelope's top-level `result` is the typed run outcome; only
    // `"completed"` is a real success. Anything else (a stop reason like
    // max-iterations or a refusal) is a retryable failure rather than a
    // success-that-looks-like-success.
    match envelope.get("result").and_then(Value::as_str) {
        Some("completed") => {}
        other => {
            return Err(ActivityFailure::retryable(format!(
                "norn did not complete (result={})",
                other.unwrap_or("<missing>")
            )));
        }
    }

    let answer = envelope
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();

    if answer.is_empty() {
        return Err(ActivityFailure::retryable(
            "norn returned empty output".to_owned(),
        ));
    }

    Ok(answer)
}

/// Derive a Norn session id that is stable per ordinal and safe as a session
/// name (the `fan:N` activity type's colon is replaced with a dash).
fn session_id_for(identity: &str, activity_type: &str) -> String {
    format!("{identity}-{}", activity_type.replace(':', "-"))
}

/// Compose the agent harness at the binary root — the ONE place this worker
/// names a concrete [`AgentHarness`](aion_integrations::AgentHarness) adapter,
/// mirroring the `aion` binary's composition root
/// (`crates/aion-cli/src/harness.rs`). The serve path drives it only through
/// the erased neutral trait ([`DynAgentHarness`]), so swapping the adapter
/// touches this function alone.
///
/// The advertised capabilities are exactly the neutral primitives the Norn
/// adapter's intervention translation supports today (`InjectMessage` +
/// `Cancel`); advertising more would promise interventions the harness rejects.
fn composed_agent_harness(norn_bin: &str) -> AgentHarnessConfig {
    let harness: Arc<dyn DynAgentHarness> = Arc::new(NornHarness::with_binary(norn_bin));
    AgentHarnessConfig::new(
        harness,
        [AGENT_ACTIVITY_TYPE],
        InterventionCapabilities::from_primitives([
            InterventionPrimitive::InjectMessage,
            InterventionPrimitive::Cancel,
        ]),
    )
}

/// Build the activity registry: one handler per `fan:N` type, each running a real
/// Norn agent step for that ordinal's prompt.
fn build_registry(norn_bin: &str, identity: &str) -> anyhow::Result<Arc<ActivityRegistry>> {
    let mut registry = ActivityRegistry::new();
    for (activity_type, prompt) in FAN_TASKS {
        let norn_bin = norn_bin.to_owned();
        let session_id = session_id_for(identity, activity_type);
        registry = registry.register_activity(
            activity_type,
            move |_input: FanInput, _context: &ActivityContext| -> HandlerFuture<'_, String> {
                let norn_bin = norn_bin.clone();
                let session_id = session_id.clone();
                let prompt = prompt.to_owned();
                Box::pin(async move {
                    tracing::info!(
                        activity = %activity_type,
                        session = %session_id,
                        "serving real Norn fan-out dispatch"
                    );
                    let answer = run_norn_step(norn_bin, session_id, prompt).await?;
                    tracing::info!(activity = %activity_type, %answer, "Norn step completed");
                    Ok(answer)
                })
            },
        )?;
    }
    Ok(Arc::new(registry))
}

/// Parsed command-line arguments.
struct Args {
    /// One or more candidate liminal listen addresses, in dial-preference order.
    candidates: Vec<String>,
    /// The worker identity announced in-band (and the Norn session-id prefix).
    identity: String,
    /// Optional readiness file written once after the first registration.
    ready_file: Option<String>,
    /// Path to the `norn` binary (default: `NORN_BIN` env, else `norn` on PATH).
    norn_bin: String,
}

/// Parse `--address` (repeatable), `--identity`, `--ready-file`, `--norn-bin`.
fn parse_args() -> anyhow::Result<Args> {
    let mut candidates: Vec<String> = Vec::new();
    let mut identity = "norn-fan-worker".to_owned();
    let mut ready_file: Option<String> = None;
    let mut norn_bin = std::env::var("NORN_BIN").unwrap_or_else(|_| "norn".to_owned());
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => match args.next() {
                Some(value) => candidates.push(value),
                None => anyhow::bail!("--address requires a value"),
            },
            "--identity" => {
                if let Some(value) = args.next() {
                    identity = value;
                }
            }
            "--ready-file" => {
                ready_file = args.next();
            }
            "--norn-bin" => {
                if let Some(value) = args.next() {
                    norn_bin = value;
                }
            }
            other => anyhow::bail!("unknown argument `{other}`"),
        }
    }
    if candidates.is_empty() {
        candidates.push("127.0.0.1:50061".to_owned());
    }
    Ok(Args {
        candidates,
        identity,
        ready_file,
        norn_bin,
    })
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = parse_args()?;
    tracing::info!(
        candidates = ?args.candidates,
        identity = %args.identity,
        norn_bin = %args.norn_bin,
        "norn-fan-worker starting"
    );

    let config = WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace("default")
        .task_queue("default")
        .identity(&args.identity)
        .max_concurrency(FAN_OUT)
        .reconnect_initial_backoff(REDIAL_INITIAL_BACKOFF)
        .reconnect_max_backoff(REDIAL_MAX_BACKOFF)
        .reconnect_max_attempts(3)
        .build()?;

    let registry = build_registry(&args.norn_bin, &args.identity)?;

    // The composed agent harness, threaded through the serve entrypoint so a
    // dispatch of the neutral `agent` activity type drives a real Norn session
    // (observable + intervenable) — the reference "proper worker" wiring. The
    // typed `fan:N` registry handlers above are untouched by it.
    let agent = composed_agent_harness(&args.norn_bin);

    // Never stop on our own; the demo ends the worker with a kill.
    let stop = AtomicBool::new(false);
    let ready_file = args.ready_file.clone();
    aion_worker::serve_with_redial(
        args.candidates,
        &config,
        &registry,
        RedialTiming::new(REDIAL_INITIAL_BACKOFF, REDIAL_MAX_BACKOFF),
        &stop,
        Some(&agent),
        move || {
            if let Some(path) = ready_file.as_ref()
                && let Err(error) = std::fs::write(path, b"connected")
            {
                tracing::error!(%error, "failed to write worker readiness file");
            }
            tracing::info!("norn-fan-worker connected and registered; serving real-AI pushes");
        },
    )?;

    Ok(())
}
