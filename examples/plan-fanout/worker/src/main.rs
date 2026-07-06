//! Rust activity worker for the plan-fanout workflow.
//!
//! It serves SIX activity types over the liminal server-push transport
//! (`serve_with_redial`, the same wiring as `incident-triage`, `agent-dev`, and
//! `norn-fan-worker`), on the DISTINCT `plan-fanout` task queue so it never
//! collides with other example workers on `default`:
//!
//! - THREE AGENT activities — `plan`, `dev`, `review` — routed through the
//!   composed [`RoleDispatchHarness`], which drives a real Norn agent in DRIVEN
//!   mode (`--protocol jsonrpc`, observable + intervenable from the ops console),
//!   picking the per-role system prompt + `--output-schema` and a DISTINCT
//!   resumable session per fan-out member. See `harness.rs` for why the custom
//!   harness is needed for dynamic-N same-type fan-out.
//! - THREE CODE activities — `validate_plan`, `tally`, `integrate` — served by the
//!   plain typed [`ActivityRegistry`]; their logic (DAG validation + layering,
//!   majority verdict, report assembly) lives in `logic.rs` and is unit-tested
//!   there. One worker serves BOTH the harness and the registry: an activity
//!   whose type is an agent type routes to the harness, everything else to the
//!   registry.
//!
//! Auth: Norn is invoked with `OPENAI_API_KEY` removed from its child env so it
//! uses the operator's ChatGPT OAuth login (see `harness.rs`).
//!
//! Usage:
//!   plan-fanout-worker [--address 127.0.0.1:PORT ...] [--identity <id>]
//!                      [--ready-file <path>] [--norn-bin <path>]

mod harness;
mod logic;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use aion_integrations::{DynAgentHarness, InterventionCapabilities, InterventionPrimitive};
use aion_worker::{
    ActivityContext, ActivityRegistry, AgentHarnessConfig, HandlerFuture, RedialTiming,
    WorkerConfig, WorkerError,
};

use crate::harness::{AGENT_ACTIVITY_TYPES, RoleDispatchHarness};
use crate::logic::{IntegrateInput, PlanOutput, ReviewOutput, integrate, tally, validate_plan};

/// The default liminal listen address the worker dials — the server's
/// `[outbox] liminal_listen_address`.
const DEFAULT_ADDRESS: &str = "127.0.0.1:50061";

/// The task queue this worker serves. DISTINCT from `default` so plan-fanout's
/// activities never collide with other workers connected to the same server.
const TASK_QUEUE: &str = "plan-fanout";

const REDIAL_INITIAL_BACKOFF: Duration = Duration::from_millis(50);
const REDIAL_MAX_BACKOFF: Duration = Duration::from_millis(500);

// --- Code-activity input wire types -----------------------------------------

/// `validate_plan` input: the planner's output carried as a JSON string plus the
/// caps the workflow resolved.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ValidateInput {
    plan: String,
    max_units: i64,
    max_fix_rounds: i64,
}

/// `tally` input: the unit and its M reviews, each carried as a JSON string.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct TallyInput {
    unit_id: String,
    reviews: Vec<String>,
}

// --- Code-activity handlers -------------------------------------------------

/// `validate_plan`: parse the planner output, validate + layer it. Never fails —
/// a malformed or unacceptable plan comes back `accepted: false` so the workflow
/// can complete with a terminal report.
fn validate_handler(
    input: ValidateInput,
    _context: &ActivityContext,
) -> HandlerFuture<'_, logic::ValidatedPlan> {
    Box::pin(async move {
        let validated = match serde_json::from_str::<PlanOutput>(&input.plan) {
            Ok(plan) => validate_plan(&plan, input.max_units, input.max_fix_rounds),
            Err(error) => logic::ValidatedPlan {
                accepted: false,
                reason: format!("planner output was not valid plan JSON: {error}"),
                rationale: String::new(),
                reviewers_per_unit: 0,
                max_fix_rounds: input.max_fix_rounds,
                layers: Vec::new(),
                units: Vec::new(),
            },
        };
        tracing::info!(
            accepted = validated.accepted,
            units = validated.units.len(),
            layers = validated.layers.len(),
            "validated plan"
        );
        Ok(validated)
    })
}

/// `tally`: majority verdict over one unit's reviews.
fn tally_handler(
    input: TallyInput,
    _context: &ActivityContext,
) -> HandlerFuture<'_, logic::TallyResult> {
    Box::pin(async move {
        let mut reviews = Vec::with_capacity(input.reviews.len());
        for raw in &input.reviews {
            let review = serde_json::from_str::<ReviewOutput>(raw).map_err(|error| {
                aion_worker::ActivityFailure::terminal(format!(
                    "review output was not valid review JSON: {error}"
                ))
            })?;
            reviews.push(review);
        }
        let result = tally(&input.unit_id, &reviews);
        tracing::info!(
            unit = %result.unit_id,
            blocked = result.blocked,
            pass = result.pass_count,
            blockers = result.blocker_count,
            "tallied reviews"
        );
        Ok(result)
    })
}

/// `integrate`: collect every settled unit into the run report.
fn integrate_handler(
    input: IntegrateInput,
    _context: &ActivityContext,
) -> HandlerFuture<'_, logic::Report> {
    Box::pin(async move {
        tracing::info!(
            disposition = %input.disposition,
            units = input.units.len(),
            "integrating run report"
        );
        Ok(integrate(input))
    })
}

/// Build the typed registry for the three CODE activities.
fn code_registry() -> Result<Arc<ActivityRegistry>, WorkerError> {
    let registry = ActivityRegistry::new()
        .register_activity("validate_plan", validate_handler)?
        .register_activity("tally", tally_handler)?
        .register_activity("integrate", integrate_handler)?;
    Ok(Arc::new(registry))
}

/// Compose the agent harness owning the `plan`/`dev`/`review` activity types.
/// The advertised capabilities are exactly the neutral primitives the Norn
/// adapter supports (`InjectMessage` + `Cancel`).
fn composed_agent_harness(norn_bin: &str) -> AgentHarnessConfig {
    let harness: Arc<dyn DynAgentHarness> = Arc::new(RoleDispatchHarness::new(norn_bin));
    AgentHarnessConfig::new(
        harness,
        AGENT_ACTIVITY_TYPES,
        InterventionCapabilities::from_primitives([
            InterventionPrimitive::InjectMessage,
            InterventionPrimitive::Cancel,
        ]),
    )
}

/// Parsed command-line arguments.
#[derive(Debug)]
struct Args {
    candidates: Vec<String>,
    identity: String,
    ready_file: Option<String>,
    norn_bin: String,
}

fn parse_args() -> anyhow::Result<Args> {
    let default_norn_bin = std::env::var("NORN_BIN").unwrap_or_else(|_| "norn".to_owned());
    parse_args_from(std::env::args().skip(1), default_norn_bin)
}

/// The argument-parsing core, fed an explicit iterator + defaults so tests
/// exercise the exact production logic. Every value-taking flag bails clearly
/// when its value is missing.
fn parse_args_from(
    args: impl IntoIterator<Item = String>,
    default_norn_bin: String,
) -> anyhow::Result<Args> {
    let mut candidates: Vec<String> = Vec::new();
    let mut identity = "plan-fanout-worker".to_owned();
    let mut ready_file: Option<String> = None;
    let mut norn_bin = default_norn_bin;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => match args.next() {
                Some(value) => candidates.push(value),
                None => anyhow::bail!("--address requires a value"),
            },
            "--identity" => match args.next() {
                Some(value) => identity = value,
                None => anyhow::bail!("--identity requires a value"),
            },
            "--ready-file" => match args.next() {
                Some(value) => ready_file = Some(value),
                None => anyhow::bail!("--ready-file requires a value"),
            },
            "--norn-bin" => match args.next() {
                Some(value) => norn_bin = value,
                None => anyhow::bail!("--norn-bin requires a value"),
            },
            other => anyhow::bail!("unknown argument `{other}`"),
        }
    }
    if candidates.is_empty() {
        candidates.push(DEFAULT_ADDRESS.to_owned());
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
        task_queue = TASK_QUEUE,
        "plan-fanout-worker starting (driven agents: plan/dev/review; code: validate_plan/tally/integrate)"
    );

    let config = WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace("default")
        .task_queue(TASK_QUEUE)
        .identity(&args.identity)
        .max_concurrency(8)
        .reconnect_initial_backoff(REDIAL_INITIAL_BACKOFF)
        .reconnect_max_backoff(REDIAL_MAX_BACKOFF)
        .reconnect_max_attempts(3)
        .build()?;

    let registry = code_registry()?;
    let agent = composed_agent_harness(&args.norn_bin);

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
            tracing::info!(
                "plan-fanout-worker connected and registered; serving plan/dev/review + validate_plan/tally/integrate"
            );
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_ADDRESS, parse_args_from};

    fn parse(args: &[&str]) -> anyhow::Result<super::Args> {
        parse_args_from(
            args.iter().map(|arg| (*arg).to_owned()),
            "norn-default".to_owned(),
        )
    }

    #[test]
    fn no_arguments_yields_the_defaults() -> anyhow::Result<()> {
        let args = parse(&[])?;
        assert_eq!(args.candidates, vec![DEFAULT_ADDRESS.to_owned()]);
        assert_eq!(args.identity, "plan-fanout-worker");
        assert_eq!(args.ready_file, None);
        assert_eq!(args.norn_bin, "norn-default");
        Ok(())
    }

    #[test]
    fn all_flags_parse_and_addresses_repeat() -> anyhow::Result<()> {
        let args = parse(&[
            "--address",
            "127.0.0.1:1111",
            "--address",
            "127.0.0.1:2222",
            "--identity",
            "worker-a",
            "--ready-file",
            "/tmp/ready",
            "--norn-bin",
            "/opt/norn",
        ])?;
        assert_eq!(
            args.candidates,
            vec!["127.0.0.1:1111".to_owned(), "127.0.0.1:2222".to_owned()]
        );
        assert_eq!(args.identity, "worker-a");
        assert_eq!(args.ready_file.as_deref(), Some("/tmp/ready"));
        assert_eq!(args.norn_bin, "/opt/norn");
        Ok(())
    }

    #[test]
    fn missing_value_bails() {
        for flag in ["--address", "--identity", "--ready-file", "--norn-bin"] {
            assert_eq!(
                parse(&[flag]).err().map(|error| error.to_string()),
                Some(format!("{flag} requires a value")),
            );
        }
    }

    #[test]
    fn unknown_argument_bails() {
        assert_eq!(
            parse(&["--bogus"]).err().map(|error| error.to_string()),
            Some("unknown argument `--bogus`".to_owned())
        );
    }
}
