//! Rust activity worker for the prospekt -> Aion incident-triage bridge demo.
//!
//! It serves the ONE `triage` activity two ways, chosen at startup:
//!
//! - **DRIVEN (default).** `triage` is an AGENT activity routed through the
//!   composed first-party [`NornHarness`] over the liminal server-push transport
//!   ([`aion_worker::serve_with_redial`], the same wiring as `agent-dev` and
//!   `norn-fan-worker`). A real Norn agent runs the triage in `--protocol
//!   jsonrpc` DRIVEN mode — its transcript streams live to the ops console and
//!   the running attempt is intervenable (inject / cancel) — constrained to the
//!   [`TriageSummary`] shape by the demo's `schemas/output.json` OUTPUT SCHEMA,
//!   so the model returns exactly `{incident_id, severity, headline,
//!   next_action}`. The Norn adapter returns that structured object verbatim as
//!   the activity result, which the Gleam workflow decodes with its existing
//!   output codec — no workflow change.
//!
//! - **PLAIN (`--plain` or `AION_TRIAGE_PLAIN=1`).** `triage` is a typed
//!   registry handler running the old deterministic severity->next-action logic.
//!   It returns the SAME structured `TriageSummary` wire shape the driven path
//!   returns, so the workflow decodes either identically — the runbook can demo
//!   both and evaluations can A/B. Default is driven.
//!
//! Auth: Norn is invoked with `OPENAI_API_KEY` REMOVED from its child
//! environment (via the adapter's `without_env`) so it uses the operator's
//! ChatGPT OAuth login — a stray ambient key would take precedence and fail.
//!
//! The prompt Norn receives is the incident document JSON (the activity input,
//! passed through verbatim by the adapter); the triage instructions ride in an
//! appended system prompt, and the required response shape is enforced by the
//! output schema — so the prompt stays SHORT and grounded in the document.
//!
//! Usage:
//!   incident-triage-worker [--address 127.0.0.1:PORT ...] [--identity <id>]
//!                          [--ready-file <path>] [--norn-bin <path>] [--plain]

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use aion_integration_norn::NornHarness;
use aion_integrations::{DynAgentHarness, InterventionCapabilities, InterventionPrimitive};
use aion_worker::{
    ActivityContext, ActivityRegistry, AgentHarnessConfig, HandlerFuture, RedialTiming,
    WorkerConfig, WorkerError,
};
use serde::{Deserialize, Serialize};

/// The activity type routed through the composed harness (driven mode) or served
/// by the typed registry (plain mode). One name, two execution paths.
const AGENT_ACTIVITY_TYPE: &str = "triage";

/// The default liminal listen address the worker dials — the server's
/// `[outbox] liminal_listen_address` in `demo-config.toml`.
const DEFAULT_ADDRESS: &str = "127.0.0.1:50061";

/// Lower bound on the reconnect backoff between candidate dials.
const REDIAL_INITIAL_BACKOFF: Duration = Duration::from_millis(50);
/// Upper bound on the reconnect backoff (a survivor may take a moment to adopt
/// the shard and bring its listener up).
const REDIAL_MAX_BACKOFF: Duration = Duration::from_millis(500);

/// The triage instructions appended to Norn's system prompt. Short and grounded:
/// the incident document JSON is the context (delivered as the run prompt), and
/// the required response shape is enforced by the output schema — so this only
/// has to say WHAT to produce, not HOW to format it.
const TRIAGE_SYSTEM_PROMPT: &str = "\
You are an incident-triage assistant. The user message is ONE incident as JSON. \
Assess it and return the structured triage output: set `incident_id` to the \
incident's `id`, `severity` to the incident's `severity`, `headline` to \
\"[<severity>] <title>\", and `next_action` to a single concrete first action \
appropriate to that severity. Base everything on the incident JSON provided; do \
not invent facts.";

/// The `TriageSummary` JSON Schema, embedded at build time. Passed to Norn as its
/// output schema so a driven run is constrained to exactly this shape. Embedding
/// (rather than resolving a path at runtime) makes the schema travel with the
/// binary and removes all cwd/path fragility; Norn treats a value starting with
/// `{` as inline JSON.
const OUTPUT_SCHEMA: &str = include_str!("../../schemas/output.json");

/// Where the incident happened. Mirrors the incident schema's `environment`
/// object; `model` (the LLM in play, if any) is optional and defaults absent.
#[derive(Debug, Deserialize, Serialize)]
struct Environment {
    binary: String,
    #[serde(default)]
    model: Option<String>,
    invocation: String,
}

/// The typed incident (plain path only). Extra prospekt-injected fields (`model`,
/// `model_version`, `forensics`) are ignored by serde since they are not modelled.
#[derive(Debug, Deserialize, Serialize)]
struct Incident {
    id: String,
    title: String,
    severity: String,
    observed: String,
    expected: String,
    environment: Environment,
    state: String,
}

/// The structured triage result returned to the workflow. The driven path
/// produces this shape via the output schema; the plain path builds it directly.
#[derive(Debug, Serialize)]
struct TriageSummary {
    incident_id: String,
    severity: String,
    headline: String,
    next_action: String,
}

/// Severity -> next action. Plain, deterministic string logic; identical to the
/// workflow's in-VM fallback so remote and local paths agree.
fn next_action_for(severity: &str) -> &'static str {
    match severity {
        "sev1" => "page on-call and open a war room now",
        "sev2" => "assign an owner and fix within the working day",
        "sev3" => "triage into the backlog for the next sprint",
        _ => "clarify severity before routing",
    }
}

/// The plain `triage` handler: deterministic severity->action, no AI. Used only
/// when the worker runs in `--plain` mode.
fn plain_triage(
    incident: Incident,
    _context: &ActivityContext,
) -> HandlerFuture<'_, TriageSummary> {
    Box::pin(async move {
        let next_action = next_action_for(&incident.severity).to_owned();
        let headline = format!("[{}] {}", incident.severity, incident.title);
        tracing::info!(incident = %incident.id, severity = %incident.severity, "serving plain triage");
        Ok(TriageSummary {
            incident_id: incident.id,
            severity: incident.severity,
            headline,
            next_action,
        })
    })
}

/// Compose the agent harness at the binary root — the ONE place this worker names
/// a concrete [`AgentHarness`](aion_integrations::AgentHarness) adapter, mirroring
/// `norn-fan-worker`, `agent-dev`, and the `aion` binary's composition root. The
/// serve path drives it only through the erased neutral trait
/// ([`DynAgentHarness`]), so swapping the adapter touches this function alone.
///
/// The harness adds, per run: the triage instructions as an appended system
/// prompt; the [`TriageSummary`] output schema (inline) so the model is
/// constrained to the exact response shape; a `{workflow_id}`-derived session id
/// (`--resume-if-exists`) so a re-dispatch after a failover RESUMES the same Norn
/// session rather than starting over; `--fast` for Norn's fast model tier; and
/// the `OPENAI_API_KEY` removal that forces the operator's ChatGPT OAuth login.
///
/// The advertised capabilities are exactly the neutral primitives the Norn
/// adapter's intervention translation supports today (`InjectMessage` +
/// `Cancel`); advertising more would promise interventions the harness rejects.
fn composed_agent_harness(norn_bin: &str) -> AgentHarnessConfig {
    let harness: Arc<dyn DynAgentHarness> = Arc::new(
        NornHarness::with_binary(norn_bin)
            .with_arg("--append-system-prompt")
            .with_arg(TRIAGE_SYSTEM_PROMPT)
            .with_arg("--output-schema")
            .with_arg(OUTPUT_SCHEMA.trim_start())
            .with_arg("--session-id")
            .with_arg("{workflow_id}-triage")
            .with_arg("--resume-if-exists")
            .with_arg("--fast")
            // Force the ChatGPT OAuth login: the project does not use API keys,
            // and a stray ambient key would take precedence and fail.
            .without_env("OPENAI_API_KEY"),
    );
    AgentHarnessConfig::new(
        harness,
        [AGENT_ACTIVITY_TYPE],
        InterventionCapabilities::from_primitives([
            InterventionPrimitive::InjectMessage,
            InterventionPrimitive::Cancel,
        ]),
    )
}

/// Parsed command-line arguments.
#[derive(Debug)]
struct Args {
    /// One or more candidate liminal listen addresses, in dial-preference order.
    candidates: Vec<String>,
    /// The worker identity announced in-band (and the Norn session-id prefix
    /// carries the run's workflow id, not this).
    identity: String,
    /// Optional readiness file written once after the first registration.
    ready_file: Option<String>,
    /// Path to the `norn` binary (default: `NORN_BIN` env, else `norn` on PATH).
    norn_bin: String,
    /// Serve `triage` with plain deterministic logic instead of a driven Norn
    /// agent. Set by `--plain` or `AION_TRIAGE_PLAIN=1`; default is driven.
    plain: bool,
}

/// Parse process arguments, defaulting `--norn-bin` from `NORN_BIN` and `plain`
/// from `AION_TRIAGE_PLAIN`.
fn parse_args() -> anyhow::Result<Args> {
    let default_norn_bin = std::env::var("NORN_BIN").unwrap_or_else(|_| "norn".to_owned());
    let default_plain = env_flag("AION_TRIAGE_PLAIN");
    parse_args_from(std::env::args().skip(1), default_norn_bin, default_plain)
}

/// Whether an environment variable is set to a truthy value (`1`/`true`/`yes`,
/// case-insensitive). Any other value — including unset — is false.
fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(false)
}

/// The argument-parsing core, fed an explicit argument iterator and defaults so
/// tests exercise the exact production logic without touching process globals.
///
/// Every value-taking flag bails with a clear message when its value is missing —
/// a silent fallback to a default would mask an operator typo.
fn parse_args_from(
    args: impl IntoIterator<Item = String>,
    default_norn_bin: String,
    default_plain: bool,
) -> anyhow::Result<Args> {
    let mut candidates: Vec<String> = Vec::new();
    let mut identity = "incident-triage-worker".to_owned();
    let mut ready_file: Option<String> = None;
    let mut norn_bin = default_norn_bin;
    let mut plain = default_plain;
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
            "--plain" => plain = true,
            "--driven" => plain = false,
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
        plain,
    })
}

/// Build the plain-mode registry: the single deterministic `triage` handler.
fn plain_registry() -> Result<Arc<ActivityRegistry>, WorkerError> {
    let registry = ActivityRegistry::new().register_activity(AGENT_ACTIVITY_TYPE, plain_triage)?;
    Ok(Arc::new(registry))
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = parse_args()?;
    let mode = if args.plain { "plain" } else { "driven (Norn)" };
    tracing::info!(
        candidates = ?args.candidates,
        identity = %args.identity,
        norn_bin = %args.norn_bin,
        mode,
        "incident-triage-worker starting"
    );

    let config = WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace("default")
        .task_queue("default")
        .identity(&args.identity)
        .max_concurrency(4)
        .reconnect_initial_backoff(REDIAL_INITIAL_BACKOFF)
        .reconnect_max_backoff(REDIAL_MAX_BACKOFF)
        .reconnect_max_attempts(3)
        .build()?;

    // Driven: an empty registry plus the composed agent harness that owns
    // `triage` (advertised via the agent-types union). Plain: the typed registry
    // serves `triage` and no harness is installed.
    let agent = (!args.plain).then(|| composed_agent_harness(&args.norn_bin));
    let registry = if args.plain {
        plain_registry()?
    } else {
        Arc::new(ActivityRegistry::new())
    };

    // Never stop on our own; the operator ends the worker with a signal.
    let stop = AtomicBool::new(false);
    let ready_file = args.ready_file.clone();
    aion_worker::serve_with_redial(
        args.candidates,
        &config,
        &registry,
        RedialTiming::new(REDIAL_INITIAL_BACKOFF, REDIAL_MAX_BACKOFF),
        &stop,
        agent.as_ref(),
        move || {
            if let Some(path) = ready_file.as_ref()
                && let Err(error) = std::fs::write(path, b"connected")
            {
                tracing::error!(%error, "failed to write worker readiness file");
            }
            tracing::info!("incident-triage-worker connected and registered; serving triage");
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Unit tests of the argument-parsing core: defaults, explicit values, the
    //! plain/driven toggle, and the strict missing-value bail for every
    //! value-taking flag.

    use super::{DEFAULT_ADDRESS, parse_args_from};

    /// Runs the parsing core on a literal argv with fixed defaults.
    fn parse(args: &[&str], default_plain: bool) -> anyhow::Result<super::Args> {
        parse_args_from(
            args.iter().map(|arg| (*arg).to_owned()),
            "norn-default".to_owned(),
            default_plain,
        )
    }

    #[test]
    fn no_arguments_yields_the_defaults() -> anyhow::Result<()> {
        let args = parse(&[], false)?;
        assert_eq!(args.candidates, vec![DEFAULT_ADDRESS.to_owned()]);
        assert_eq!(args.identity, "incident-triage-worker");
        assert_eq!(args.ready_file, None);
        assert_eq!(args.norn_bin, "norn-default");
        assert!(!args.plain, "default is driven");
        Ok(())
    }

    #[test]
    fn all_flags_with_values_parse_and_addresses_repeat() -> anyhow::Result<()> {
        let args = parse(
            &[
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
                "--plain",
            ],
            false,
        )?;
        assert_eq!(
            args.candidates,
            vec!["127.0.0.1:1111".to_owned(), "127.0.0.1:2222".to_owned()]
        );
        assert_eq!(args.identity, "worker-a");
        assert_eq!(args.ready_file.as_deref(), Some("/tmp/ready"));
        assert_eq!(args.norn_bin, "/opt/norn");
        assert!(args.plain);
        Ok(())
    }

    #[test]
    fn plain_env_default_is_overridable_by_driven_flag() -> anyhow::Result<()> {
        // `AION_TRIAGE_PLAIN=1` seeds `default_plain = true`; an explicit
        // `--driven` on the argv wins.
        assert!(parse(&[], true)?.plain, "env default plain holds");
        assert!(!parse(&["--driven"], true)?.plain, "--driven overrides env");
        assert!(
            parse(&["--plain"], false)?.plain,
            "--plain overrides default"
        );
        Ok(())
    }

    #[test]
    fn every_value_taking_flag_bails_when_its_value_is_missing() {
        for flag in ["--address", "--identity", "--ready-file", "--norn-bin"] {
            assert_eq!(
                parse(&[flag], false).err().map(|error| error.to_string()),
                Some(format!("{flag} requires a value")),
                "a trailing value-less {flag} must bail"
            );
        }
    }

    #[test]
    fn unknown_argument_bails() {
        assert_eq!(
            parse(&["--bogus"], false)
                .err()
                .map(|error| error.to_string()),
            Some("unknown argument `--bogus`".to_owned())
        );
    }
}
