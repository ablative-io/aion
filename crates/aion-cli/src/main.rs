//! The `aion` command line: workflow operations over gRPC and the embedded
//! Aion workflow server (`aion server`).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use aion_client::{Client, ClientBuilder, ClientError, ListPage, StartOptions};
use aion_core::{WorkflowFilter, WorkflowStatus};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;

use crate::output::{
    AcknowledgementOutput, QueryOutput, describe_output, print_json, reopen_output, start_output,
    to_value,
};
use crate::payload::{
    empty_query_payload, json_payload, parse_run_id, parse_status, parse_workflow_id,
    payload_to_json,
};

mod check_deterministic;
mod codegen;
mod deploy;
mod dev;
mod generate;
mod harness;
mod input;
mod inspect;
mod new;
mod output;
mod package;
mod payload;
mod render;
mod server;
#[cfg(all(feature = "norn", feature = "liminal-transport"))]
mod worker;

const DEFAULT_ENDPOINT: &str = "127.0.0.1:50051";
const DEFAULT_NAMESPACE: &str = "default";
const DEFAULT_SUBJECT: &str = "cli-user";
const QUERY_DEADLINE: Duration = Duration::from_secs(5);

/// Aion workflow command line.
#[derive(Debug, Parser)]
#[command(
    name = "aion",
    version,
    about = "Operate Aion workflows over gRPC and run the Aion workflow server"
)]
struct Cli {
    /// gRPC server endpoint. A missing scheme is interpreted as http://.
    #[arg(long, global = true, default_value = DEFAULT_ENDPOINT)]
    endpoint: String,
    /// Workflow namespace used for operations.
    #[arg(long, global = true, default_value = DEFAULT_NAMESPACE)]
    namespace: String,
    /// Caller subject metadata sent to the server.
    #[arg(long, global = true, default_value = DEFAULT_SUBJECT)]
    subject: String,
    /// Bearer token for authenticated servers. Overrides the `AION_TOKEN`
    /// environment variable; when both are absent the development
    /// header paths apply.
    #[arg(long, global = true)]
    token: Option<String>,
    /// Print formatted, human-readable JSON.
    #[arg(long, global = true)]
    pretty: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the Aion workflow server (HTTP, gRPC, WebSocket, remote workers).
    ///
    /// Loads configuration from `--config`, the environment, and command-line
    /// overrides, serves until a termination signal arrives, then drains
    /// in-flight activities gracefully before exiting.
    Server(server::ServerArgs),
    /// The instant authoring loop: watch a workflow project and hot-load on save.
    ///
    /// Watches the project's `src/` tree and, on every save, rebuilds and
    /// type-checks it through the external `gleam` binary, repackages it into a
    /// content-hash-versioned `.aion`, and hot-loads that version into the
    /// running server named by `--endpoint` — with no engine restart. A run
    /// already in-flight keeps executing on the immutable version it started
    /// with; only fresh runs pick up the reload. The `gleam` binary path is
    /// required (`--gleam-path`); there is no default. An optional
    /// `--debounce-ms` coalesces an editor's multi-event save into one rebuild.
    Dev(dev::DevArgs),
    /// Run a remote worker that serves agent activities over the liminal transport.
    ///
    /// Connects IN to one or more deployed `aion server`
    /// `outbox.liminal_listen_address` listeners, self-registers, and serves pushed
    /// dispatches — installing the composed agent harness so agent activities run
    /// live, and migrating to a survivor listener when the owner of a shard dies.
    #[cfg(all(feature = "norn", feature = "liminal-transport"))]
    #[command(subcommand)]
    Worker(WorkerCommand),
    /// Operate a running Aion server or package workflows locally.
    #[command(flatten)]
    Client(ClientCommand),
}

/// Subcommands under `aion worker`.
#[cfg(all(feature = "norn", feature = "liminal-transport"))]
#[derive(Debug, Subcommand)]
enum WorkerCommand {
    /// Serve agent activities over liminal until interrupted.
    Serve(worker::WorkerServeArgs),
}

#[derive(Debug, Subcommand)]
enum ClientCommand {
    /// Scaffold a new Aion workflow project from a template.
    ///
    /// Creates `<name>/` in the current directory: a buildable Gleam
    /// workflow with a typed entry function, `workflow.toml`, JSON Schemas,
    /// a ready development server config (`aion.toml`), a `.gitignore`, and
    /// a README with the exact next steps. Runs entirely locally and
    /// refuses to write into a non-empty directory.
    ///
    /// Templates: `hello-world` (default; minimal start → complete),
    /// `approval-flow` (signal vs durable timeout race plus a status
    /// query), `saga` (worker-served activities, workflow-driven retries, and
    /// refund compensation), `agent` (a durable agent loop — scout → act →
    /// verify → signal-gated human review, parameterised by prompts;
    /// `--worker rust` required), and `dev-pipeline` (the durable dev
    /// pipeline; `--worker rust` required). `--worker rust` additionally
    /// scaffolds a `worker/` crate serving the template's activities.
    New(new::NewArgs),
    /// Generate Gleam types and JSON codecs from a project's JSON Schemas.
    ///
    /// Runs entirely locally: reads `workflow.toml` and every
    /// `schemas/*.json` in the project root, and writes one deterministic
    /// module (`src/<package>_io.gleam`, do not edit) containing a Gleam
    /// type plus an encoder/decoder pair per schema — the schemas stay the
    /// single source of truth and the codecs cannot drift. Schema constructs
    /// outside the supported subset fail loudly with the file and JSON
    /// pointer; see docs/guides/codegen.md for the subset table.
    Codegen {
        /// Workflow project root containing `workflow.toml` and `schemas/`.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Verify the generated module on disk is current instead of
        /// writing; exits non-zero naming the drifted file (CI gate).
        #[arg(long)]
        check: bool,
    },
    /// Generate every per-activity artifact from a package's typed declarations.
    ///
    /// Runs locally and drives the Gleam toolchain: reads the declarations its
    /// `src/<package>_activities.gleam` returns from `manifest()`, then writes
    /// the io types/codecs, the typed codec wrappers, the `activity.new`
    /// wrappers, the per-tier worker module, the remote wire-compat golden, and
    /// the `workflow.toml` activities list — all do-not-edit and derived, so an
    /// activity is declared once instead of hand-mirrored across files. With
    /// `--check`, regenerates everything in memory and exits non-zero if any
    /// on-disk generated file differs (CI drift gate).
    Generate {
        /// Workflow project root containing `gleam.toml`, `workflow.toml`,
        /// `schemas/`, and `src/<package>_activities.gleam`.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Verify every generated file on disk is current instead of writing;
        /// exits non-zero naming the drifted file (CI gate).
        #[arg(long)]
        check: bool,
    },
    /// Statically check a workflow project for determinism violations.
    ///
    /// Runs entirely locally: reads `workflow.toml` and each declared
    /// workflow's `src/<entry_module>.gleam`, and flags any wall-clock or
    /// entropy call reachable from workflow code — the determinism boundary as
    /// a CI gate. Exits non-zero, naming every offending call and the
    /// `workflow.now()` / `workflow.random()` substitute to use, when any
    /// violation is found.
    Check {
        /// Workflow project root containing `workflow.toml` and `src/`.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Run the determinism gate. Required: `check` performs the
        /// determinism analysis only.
        #[arg(long)]
        deterministic: bool,
    },
    /// Emit a valid input skeleton derived from a workflow's input type.
    ///
    /// Runs entirely locally: reads the workflow's `input_schema` from
    /// `workflow.toml` — the same JSON Schema the input codec is generated
    /// from — and prints a structurally-valid JSON document so an input is
    /// never hand-written from scratch. Required properties carry type-shaped
    /// placeholders and optional properties are omitted; no values are
    /// defaulted.
    Input {
        /// Workflow type (the workflow's `entry_module`).
        workflow_type: String,
        /// Workflow project root containing `workflow.toml` and `schemas/`.
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Package an already-built Gleam workflow project into `.aion` archives.
    ///
    /// Runs entirely locally: reads `workflow.toml` in the project root and
    /// never connects to a server. Pass `--build` to run `gleam build` first.
    Package {
        /// Workflow project root containing `workflow.toml`.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Archive output path, resolved against the current directory.
        /// Only valid when the project declares exactly one workflow.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Run `gleam build` in the project before packaging.
        #[arg(long)]
        build: bool,
    },
    /// Deploy a built `.aion` archive into a running server (operator API).
    ///
    /// Requires the server's `[deploy]` surface to be enabled and the caller
    /// to hold the deploy grant (`deploy` token claim, or the
    /// `x-aion-deploy` development header the CLI sends automatically).
    Deploy {
        /// Path to a complete `.aion` archive.
        archive: PathBuf,
    },
    /// List every loaded workflow version with its routing flag (operator API).
    Versions {
        /// Show only versions of this workflow type.
        #[arg(long)]
        workflow_type: Option<String>,
    },
    /// Re-point routing for a workflow type at an already-loaded version
    /// (rollback / roll-forward; operator API).
    Route {
        /// Logical workflow type.
        workflow_type: String,
        /// Content hash of the already-loaded target version.
        content_hash: String,
    },
    /// Unload a non-routed, unpinned workflow version (operator API).
    Unload {
        /// Logical workflow type.
        workflow_type: String,
        /// Content hash of the version to unload.
        content_hash: String,
    },
    /// Operate workflows on a running Aion server.
    #[command(flatten)]
    Remote(RemoteCommand),
}

#[derive(Debug, Subcommand)]
enum RemoteCommand {
    /// Start a workflow execution.
    Start {
        /// Workflow type registered with the server.
        workflow_type: String,
        /// JSON input payload for the workflow.
        #[arg(long, default_value = "null")]
        input: String,
    },
    /// Send a signal to the latest run of a workflow.
    Signal {
        /// Workflow identifier.
        workflow_id: String,
        /// Signal name.
        signal_name: String,
        /// Specific run identifier. Defaults to the latest run when omitted.
        #[arg(long)]
        run_id: Option<String>,
        /// JSON signal payload.
        #[arg(long, default_value = "null")]
        payload: String,
    },
    /// Query the latest run of a workflow.
    Query {
        /// Workflow identifier.
        workflow_id: String,
        /// Query name.
        query_name: String,
        /// Specific run identifier. Defaults to the latest run when omitted.
        #[arg(long)]
        run_id: Option<String>,
    },
    /// Request workflow cancellation.
    Cancel {
        /// Workflow identifier.
        workflow_id: String,
        /// Cancellation reason.
        #[arg(long, default_value = "cancelled by aion")]
        reason: String,
        /// Specific run identifier. Defaults to the latest run when omitted.
        #[arg(long)]
        run_id: Option<String>,
    },
    /// Reopen a terminal-reopenable (Failed or Cancelled) run and re-drive it.
    Reopen {
        /// Workflow identifier.
        workflow_id: String,
        /// Specific run identifier. Defaults to the latest run when omitted.
        #[arg(long)]
        run_id: Option<String>,
    },
    /// List workflow executions.
    List {
        /// Optional workflow status filter. Accepted values: running, completed, failed, cancelled, timed-out, continued-as-new.
        #[arg(long, value_parser = parse_status)]
        status: Option<WorkflowStatus>,
    },
    /// Describe a workflow execution and its history.
    Describe {
        /// Workflow identifier.
        workflow_id: String,
        /// Specific run identifier. Defaults to the latest run when omitted.
        #[arg(long)]
        run_id: Option<String>,
        /// Show raw payload byte arrays instead of decoded payloads.
        #[arg(long)]
        raw: bool,
    },
    /// Time-travel over a recorded run's event-store oplog.
    ///
    /// Reads the run's existing history over the same `describe` read every
    /// other command uses — no debug-only log and no second store. For each
    /// recorded event it prints the workflow-visible state projection and the
    /// recorded `now()` for that step; `random()` is surfaced as a run-level
    /// draw-ordinal projection (deterministic, never recorded, with a
    /// workflow-code-dependent per-step count), not a per-step value. On a
    /// non-determinism fault it prints the exact divergent command (expected
    /// vs found at the sequence) the resolver already computed.
    ///
    /// With `--from <seq> --mock <json>` it runs a what-if re-run from the
    /// chosen event with a mocked outcome via the existing replay path and
    /// reports the resulting path. The what-if is entirely in-memory and
    /// read-only; it never writes the event log. `--from` and `--mock` are
    /// required together — a what-if never defaults its fork point or outcome.
    /// `--mock` shape: `{"kind":"activity_completed","result":<json>}`,
    /// `{"kind":"activity_failed","message":"..."}`,
    /// `{"kind":"child_completed","result":<json>}`,
    /// `{"kind":"child_failed","message":"..."}`,
    /// `{"kind":"signal_delivered","payload":<json>}`, or
    /// `{"kind":"timer_fired"}`.
    Inspect {
        /// Workflow identifier.
        workflow_id: String,
        /// Specific run identifier. Defaults to the latest recorded run.
        #[arg(long)]
        run_id: Option<String>,
        /// Fork the what-if re-run from this recorded event sequence.
        #[arg(long)]
        from: Option<u64>,
        /// Mocked outcome JSON for the what-if fork (see the command help).
        #[arg(long)]
        mock: Option<String>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        // The server owns its own reporting contract: tracing-logged errors
        // and the operations exit codes (2 = config, 130 = forced).
        Command::Server(ref args) => {
            // Report the agent harness composed into this binary at the root
            // (§3A.3): the default build ships Norn out-of-box; a
            // `--no-default-features` build reports none. The worker drives
            // whichever harness is compiled in through the neutral trait.
            harness::announce_composed_harness();
            // `--open` is a CLI-only nicety: resolve the served URL from the
            // effective config and, once the listener is reachable, open it in
            // the browser. Best-effort — a failure to open never blocks the run.
            if args.open() {
                server::spawn_browser_open(&args.clone().into());
            }
            aion_server::run(args.clone().into()).await
        }
        Command::Dev(ref args) => {
            // The dev loop runs locally and drives the running server over the
            // same operator deploy RPC `aion deploy` uses; it reports through
            // the one client rendering contract.
            let result = dev::run(args, deploy_target(&cli))
                .await
                .and_then(|value| print_json(&value, cli.pretty));
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{}", render::render_error(&error));
                    ExitCode::FAILURE
                }
            }
        }
        #[cfg(all(feature = "norn", feature = "liminal-transport"))]
        Command::Worker(WorkerCommand::Serve(ref args)) => {
            // The production worker-serve entrypoint: install the composed agent
            // harness and drive the reconnect-to-survivor serve loop. Blocks until
            // the loop ends; a serve failure reports through the client contract.
            match worker::serve(args) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{}", render::render_error(&error));
                    ExitCode::FAILURE
                }
            }
        }
        Command::Client(ref command) => {
            let result = run(&cli, command)
                .await
                .and_then(|value| print_json(&value, cli.pretty));
            match result {
                Ok(()) => ExitCode::SUCCESS,
                // The single rendering contract for every client subcommand:
                // the taxonomy class, server detail, error type, and hint go
                // to stderr; the process exits 1.
                Err(error) => {
                    eprintln!("{}", render::render_error(&error));
                    ExitCode::FAILURE
                }
            }
        }
    }
}

/// Dispatches the parsed client command, connecting to the server only for
/// remote commands. `package` is local-only and never constructs a client;
/// the deploy subcommands drive the operator `DeployService` directly and
/// never touch the caller SDK.
async fn run(cli: &Cli, command: &ClientCommand) -> Result<Value> {
    match command {
        ClientCommand::New(args) => new::run(args),
        ClientCommand::Codegen { path, check } => codegen::run(path, *check),
        ClientCommand::Generate { path, check } => generate::run(path, *check),
        ClientCommand::Check {
            path,
            deterministic,
        } => check_deterministic::run(path, *deterministic),
        ClientCommand::Input {
            workflow_type,
            path,
        } => input::run(path, workflow_type),
        ClientCommand::Package { path, out, build } => package::run(path, out.as_deref(), *build),
        ClientCommand::Deploy { archive } => deploy::deploy(&deploy_target(cli), archive).await,
        ClientCommand::Versions { workflow_type } => {
            deploy::versions(&deploy_target(cli), workflow_type.as_deref()).await
        }
        ClientCommand::Route {
            workflow_type,
            content_hash,
        } => deploy::route(&deploy_target(cli), workflow_type, content_hash).await,
        ClientCommand::Unload {
            workflow_type,
            content_hash,
        } => deploy::unload(&deploy_target(cli), workflow_type, content_hash).await,
        ClientCommand::Remote(command) => {
            let client = build_client(cli).await?;
            execute(&client, command).await
        }
    }
}

fn deploy_target(cli: &Cli) -> deploy::DeployTarget {
    deploy::DeployTarget::new(
        normalize_endpoint(&cli.endpoint),
        cli.subject.clone(),
        deploy::resolve_token(cli.token.clone()),
    )
}

async fn build_client(cli: &Cli) -> Result<Client> {
    ClientBuilder::new(normalize_endpoint(&cli.endpoint))
        .with_namespace(cli.namespace.clone())
        .with_subject(cli.subject.clone())
        .with_authorized_namespaces([cli.namespace.clone()])
        .build()
        .await
        .context("failed to connect to Aion server")
}

async fn execute(client: &Client, command: &RemoteCommand) -> Result<Value> {
    match command {
        RemoteCommand::Start {
            workflow_type,
            input,
        } => start_workflow(client, workflow_type, input).await,
        RemoteCommand::Signal {
            workflow_id,
            signal_name,
            run_id,
            payload,
        } => signal_workflow(client, workflow_id, signal_name, run_id.as_deref(), payload).await,
        RemoteCommand::Query {
            workflow_id,
            query_name,
            run_id,
        } => query_workflow(client, workflow_id, query_name, run_id.as_deref()).await,
        RemoteCommand::Cancel {
            workflow_id,
            reason,
            run_id,
        } => cancel_workflow(client, workflow_id, reason, run_id.as_deref()).await,
        RemoteCommand::Reopen {
            workflow_id,
            run_id,
        } => reopen_workflow(client, workflow_id, run_id.as_deref()).await,
        RemoteCommand::List { status } => list_workflows(client, *status).await,
        RemoteCommand::Describe {
            workflow_id,
            run_id,
            raw,
        } => describe_workflow(client, workflow_id, run_id.as_deref(), *raw).await,
        RemoteCommand::Inspect {
            workflow_id,
            run_id,
            from,
            mock,
        } => {
            inspect::run(
                client,
                workflow_id,
                run_id.as_deref(),
                *from,
                mock.as_deref(),
            )
            .await
        }
    }
}

async fn start_workflow(client: &Client, workflow_type: &str, input: &str) -> Result<Value> {
    let input = json_payload(input).context("invalid --input JSON")?;
    let handle = client
        .start(workflow_type.to_owned(), input, StartOptions::default())
        .await
        .context("failed to start workflow")?;
    to_value(start_output(&handle))
}

async fn signal_workflow(
    client: &Client,
    workflow_id: &str,
    signal_name: &str,
    run_id: Option<&str>,
    payload: &str,
) -> Result<Value> {
    let workflow_id = parse_workflow_id(workflow_id)?;
    let run_id = run_id.map(parse_run_id).transpose()?;
    let payload = json_payload(payload).context("invalid --payload JSON")?;
    client
        .signal(
            &workflow_id,
            run_id.as_ref(),
            signal_name.to_owned(),
            payload,
        )
        .await
        .context("failed to signal workflow")?;
    to_value(AcknowledgementOutput {
        workflow_id: &workflow_id.to_string(),
        accepted: true,
    })
}

async fn query_workflow(
    client: &Client,
    workflow_id: &str,
    query_name: &str,
    run_id: Option<&str>,
) -> Result<Value> {
    let workflow_id = parse_workflow_id(workflow_id)?;
    let run_id = run_id.map(parse_run_id).transpose()?;
    let payload = client
        .query(
            &workflow_id,
            run_id.as_ref(),
            query_name.to_owned(),
            empty_query_payload(),
            QUERY_DEADLINE,
        )
        .await
        .context("failed to query workflow")?;
    to_value(QueryOutput {
        result: payload_to_json(&payload)?,
    })
}

async fn cancel_workflow(
    client: &Client,
    workflow_id: &str,
    reason: &str,
    run_id: Option<&str>,
) -> Result<Value> {
    let workflow_id = parse_workflow_id(workflow_id)?;
    let run_id = run_id.map(parse_run_id).transpose()?;
    client
        .cancel(&workflow_id, run_id.as_ref(), reason.to_owned())
        .await
        .context("failed to cancel workflow")?;
    to_value(AcknowledgementOutput {
        workflow_id: &workflow_id.to_string(),
        accepted: true,
    })
}

async fn reopen_workflow(
    client: &Client,
    workflow_id: &str,
    run_id: Option<&str>,
) -> Result<Value> {
    let workflow_id = parse_workflow_id(workflow_id)?;
    let run_id = run_id.map(parse_run_id).transpose()?;
    let outcome = client
        .reopen(&workflow_id, run_id.as_ref())
        .await
        .map_err(|error| reopen_error(&workflow_id.to_string(), error))?;
    to_value(reopen_output(&workflow_id.to_string(), &outcome))
}

/// Render a reopen failure as a typed, operator-facing message — never a raw
/// transport error. `InvalidState` names the precondition ("not reopenable"),
/// `NotFound` is a plain "not found"; everything else keeps the client detail.
fn reopen_error(workflow_id: &str, error: ClientError) -> anyhow::Error {
    match error {
        ClientError::InvalidState { detail } => {
            anyhow::anyhow!(
                "workflow {workflow_id} is not reopenable: {}",
                detail.message
            )
        }
        ClientError::NotFound { .. } => anyhow::anyhow!("workflow {workflow_id} not found"),
        other => anyhow::anyhow!("failed to reopen workflow {workflow_id}: {other}"),
    }
}

async fn list_workflows(client: &Client, status: Option<WorkflowStatus>) -> Result<Value> {
    let filter = WorkflowFilter {
        status,
        ..WorkflowFilter::default()
    };
    let summaries = client
        .list(&filter, ListPage::default())
        .await
        .context("failed to list workflows")?;
    to_value(summaries)
}

async fn describe_workflow(
    client: &Client,
    workflow_id: &str,
    run_id: Option<&str>,
    raw: bool,
) -> Result<Value> {
    let workflow_id = parse_workflow_id(workflow_id)?;
    let run_id = run_id.map(parse_run_id).transpose()?;
    let description = client
        .describe(&workflow_id, run_id.as_ref())
        .await
        .context("failed to describe workflow")?;
    describe_output(description.summary, description.history, raw)
}

fn normalize_endpoint(endpoint: &str) -> String {
    if endpoint.contains("://") {
        endpoint.to_owned()
    } else {
        format!("http://{endpoint}")
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use aion_server::config::CliOverrides;
    use clap::{CommandFactory, Parser};

    use super::{Cli, ClientCommand, Command, RemoteCommand, normalize_endpoint};

    const WORKFLOW_ID: &str = "00000000-0000-0000-0000-000000000001";
    const RUN_ID: &str = "00000000-0000-0000-0000-000000000002";

    #[test]
    fn describe_accepts_optional_run_id() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from(["aion", "describe", WORKFLOW_ID, "--run-id", RUN_ID])?;

        let Command::Client(ClientCommand::Remote(RemoteCommand::Describe {
            workflow_id,
            run_id,
            raw,
        })) = cli.command
        else {
            anyhow::bail!("expected describe command");
        };
        assert_eq!(workflow_id, WORKFLOW_ID);
        assert_eq!(run_id.as_deref(), Some(RUN_ID));
        assert!(!raw);
        Ok(())
    }

    #[test]
    fn describe_accepts_raw_payload_flag() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from(["aion", "describe", WORKFLOW_ID, "--raw"])?;

        let Command::Client(ClientCommand::Remote(RemoteCommand::Describe { raw, .. })) =
            cli.command
        else {
            anyhow::bail!("expected describe command");
        };
        assert!(raw);
        Ok(())
    }

    #[test]
    fn reopen_parses_workflow_id_and_optional_run_id() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from(["aion", "reopen", WORKFLOW_ID])?;
        let Command::Client(ClientCommand::Remote(RemoteCommand::Reopen {
            workflow_id,
            run_id,
        })) = cli.command
        else {
            anyhow::bail!("expected reopen command");
        };
        assert_eq!(workflow_id, WORKFLOW_ID);
        assert!(run_id.is_none());

        let cli = Cli::try_parse_from(["aion", "reopen", WORKFLOW_ID, "--run-id", RUN_ID])?;
        let Command::Client(ClientCommand::Remote(RemoteCommand::Reopen { run_id, .. })) =
            cli.command
        else {
            anyhow::bail!("expected reopen command");
        };
        assert_eq!(run_id.as_deref(), Some(RUN_ID));
        Ok(())
    }

    #[test]
    fn reopen_help_documents_the_subcommand() -> anyhow::Result<()> {
        let mut command = Cli::command();
        let Some(reopen) = command.find_subcommand_mut("reopen") else {
            anyhow::bail!("reopen subcommand should be registered");
        };
        let help = reopen.render_long_help().to_string();
        assert!(help.contains("--run-id"));
        assert!(help.contains("reopen"));
        Ok(())
    }

    #[test]
    fn new_parses_name_template_and_worker() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from([
            "aion",
            "new",
            "my_flow",
            "--template",
            "saga",
            "--worker",
            "rust",
        ])?;
        let Command::Client(ClientCommand::New(_)) = cli.command else {
            anyhow::bail!("expected new command");
        };

        // The template defaults to hello-world and --worker is optional.
        let cli = Cli::try_parse_from(["aion", "new", "my_flow"])?;
        let Command::Client(ClientCommand::New(_)) = cli.command else {
            anyhow::bail!("expected new command");
        };

        // Unknown templates and worker languages are parse errors.
        assert!(Cli::try_parse_from(["aion", "new", "x", "--template", "nope"]).is_err());
        assert!(Cli::try_parse_from(["aion", "new", "x", "--worker", "cobol"]).is_err());
        Ok(())
    }

    #[test]
    fn new_help_documents_templates_and_refusal() -> anyhow::Result<()> {
        let mut command = Cli::command();
        let Some(new) = command.find_subcommand_mut("new") else {
            anyhow::bail!("new subcommand should be registered");
        };
        let help = new.render_long_help().to_string();

        assert!(help.contains("--template"));
        assert!(help.contains("hello-world"));
        assert!(help.contains("approval-flow"));
        assert!(help.contains("saga"));
        assert!(help.contains("--worker"));
        assert!(help.contains("non-empty directory"));
        Ok(())
    }

    #[test]
    fn codegen_defaults_to_current_directory_without_check() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from(["aion", "codegen"])?;

        let Command::Client(ClientCommand::Codegen { path, check }) = cli.command else {
            anyhow::bail!("expected codegen command");
        };
        assert_eq!(path, Path::new("."));
        assert!(!check);
        Ok(())
    }

    #[test]
    fn codegen_accepts_path_and_check() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from(["aion", "codegen", "examples/hello-world", "--check"])?;

        let Command::Client(ClientCommand::Codegen { path, check }) = cli.command else {
            anyhow::bail!("expected codegen command");
        };
        assert_eq!(path, Path::new("examples/hello-world"));
        assert!(check);
        Ok(())
    }

    #[test]
    fn codegen_help_documents_the_contract() -> anyhow::Result<()> {
        let mut command = Cli::command();
        let Some(codegen) = command.find_subcommand_mut("codegen") else {
            anyhow::bail!("codegen subcommand should be registered");
        };
        let help = codegen.render_long_help().to_string();

        assert!(help.contains("--check"));
        assert!(help.contains("schemas/*.json"));
        assert!(help.contains("do not edit"));
        assert!(help.contains("fail loudly"));
        Ok(())
    }

    #[test]
    fn package_defaults_to_current_directory_without_build_or_out() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from(["aion", "package"])?;

        let Command::Client(ClientCommand::Package { path, out, build }) = cli.command else {
            anyhow::bail!("expected package command");
        };
        assert_eq!(path, Path::new("."));
        assert!(out.is_none());
        assert!(!build);
        Ok(())
    }

    #[test]
    fn package_accepts_path_out_and_build() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from([
            "aion",
            "package",
            "examples/hello-world",
            "--out",
            "dist/hello.aion",
            "--build",
        ])?;

        let Command::Client(ClientCommand::Package { path, out, build }) = cli.command else {
            anyhow::bail!("expected package command");
        };
        assert_eq!(path, Path::new("examples/hello-world"));
        assert_eq!(out.as_deref(), Some(Path::new("dist/hello.aion")));
        assert!(build);
        Ok(())
    }

    #[test]
    fn package_help_documents_local_only_behaviour() -> anyhow::Result<()> {
        let mut command = Cli::command();
        let Some(package) = command.find_subcommand_mut("package") else {
            anyhow::bail!("package subcommand should be registered");
        };
        let help = package.render_long_help().to_string();

        assert!(help.contains("--build"));
        assert!(help.contains("--out"));
        assert!(help.contains("never connects to a server"));
        Ok(())
    }

    #[test]
    fn check_parses_path_and_requires_the_deterministic_flag() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from(["aion", "check", "examples/order-saga", "--deterministic"])?;
        let Command::Client(ClientCommand::Check {
            path,
            deterministic,
        }) = cli.command
        else {
            anyhow::bail!("expected check command");
        };
        assert_eq!(path, Path::new("examples/order-saga"));
        assert!(deterministic);

        // Path defaults to the current directory; the flag defaults off.
        let cli = Cli::try_parse_from(["aion", "check"])?;
        let Command::Client(ClientCommand::Check {
            path,
            deterministic,
        }) = cli.command
        else {
            anyhow::bail!("expected check command");
        };
        assert_eq!(path, Path::new("."));
        assert!(!deterministic);
        Ok(())
    }

    #[test]
    fn input_parses_workflow_type_and_path() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from(["aion", "input", "order_saga", "examples/order-saga"])?;
        let Command::Client(ClientCommand::Input {
            workflow_type,
            path,
        }) = cli.command
        else {
            anyhow::bail!("expected input command");
        };
        assert_eq!(workflow_type, "order_saga");
        assert_eq!(path, Path::new("examples/order-saga"));

        // The path defaults to the current directory.
        let cli = Cli::try_parse_from(["aion", "input", "order_saga"])?;
        let Command::Client(ClientCommand::Input { path, .. }) = cli.command else {
            anyhow::bail!("expected input command");
        };
        assert_eq!(path, Path::new("."));
        Ok(())
    }

    #[test]
    fn check_help_documents_the_determinism_gate() -> anyhow::Result<()> {
        let mut command = Cli::command();
        let Some(check) = command.find_subcommand_mut("check") else {
            anyhow::bail!("check subcommand should be registered");
        };
        let help = check.render_long_help().to_string();
        assert!(help.contains("--deterministic"));
        assert!(help.contains("wall-clock"));
        assert!(help.contains("Exits non-zero"));
        Ok(())
    }

    #[test]
    fn inspect_parses_workflow_run_from_and_mock() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from(["aion", "inspect", WORKFLOW_ID])?;
        let Command::Client(ClientCommand::Remote(RemoteCommand::Inspect {
            workflow_id,
            run_id,
            from,
            mock,
        })) = cli.command
        else {
            anyhow::bail!("expected inspect command");
        };
        assert_eq!(workflow_id, WORKFLOW_ID);
        assert!(run_id.is_none());
        assert!(from.is_none());
        assert!(mock.is_none());

        let cli = Cli::try_parse_from(["aion", "inspect", WORKFLOW_ID, "--run-id", RUN_ID])?;
        let Command::Client(ClientCommand::Remote(RemoteCommand::Inspect { run_id, .. })) =
            cli.command
        else {
            anyhow::bail!("expected inspect command");
        };
        assert_eq!(run_id.as_deref(), Some(RUN_ID));

        let cli = Cli::try_parse_from([
            "aion",
            "inspect",
            WORKFLOW_ID,
            "--from",
            "3",
            "--mock",
            r#"{"kind":"timer_fired"}"#,
        ])?;
        let Command::Client(ClientCommand::Remote(RemoteCommand::Inspect { from, mock, .. })) =
            cli.command
        else {
            anyhow::bail!("expected inspect command");
        };
        assert_eq!(from, Some(3));
        assert_eq!(mock.as_deref(), Some(r#"{"kind":"timer_fired"}"#));
        Ok(())
    }

    #[test]
    fn inspect_help_documents_oplog_read_and_what_if() -> anyhow::Result<()> {
        let mut command = Cli::command();
        let Some(inspect) = command.find_subcommand_mut("inspect") else {
            anyhow::bail!("inspect subcommand should be registered");
        };
        let help = inspect.render_long_help().to_string();

        assert!(help.contains("no debug-only log and no second store"));
        assert!(help.contains("random()"));
        assert!(help.contains("divergent command"));
        assert!(help.contains("--from"));
        assert!(help.contains("--mock"));
        assert!(help.contains("what-if"));
        Ok(())
    }

    #[test]
    fn describe_help_documents_raw_payload_flag() -> anyhow::Result<()> {
        let mut command = Cli::command();
        let Some(describe) = command.find_subcommand_mut("describe") else {
            anyhow::bail!("describe subcommand should be registered");
        };
        let help = describe.render_long_help().to_string();

        assert!(help.contains("--raw"));
        assert!(help.contains("Show raw payload byte arrays instead of decoded payloads"));
        Ok(())
    }

    #[test]
    fn workflow_operations_allow_omitted_run_id() -> anyhow::Result<()> {
        let commands = [
            vec!["aion", "describe", WORKFLOW_ID],
            vec!["aion", "signal", WORKFLOW_ID, "poke", "--payload", "{}"],
            vec!["aion", "cancel", WORKFLOW_ID],
            vec!["aion", "reopen", WORKFLOW_ID],
            vec!["aion", "query", WORKFLOW_ID, "state"],
        ];

        for args in commands {
            let cli = Cli::try_parse_from(args)?;
            match cli.command {
                Command::Client(ClientCommand::Remote(
                    RemoteCommand::Describe { run_id, .. }
                    | RemoteCommand::Signal { run_id, .. }
                    | RemoteCommand::Cancel { run_id, .. }
                    | RemoteCommand::Reopen { run_id, .. }
                    | RemoteCommand::Query { run_id, .. },
                )) => assert!(run_id.is_none()),
                #[cfg(all(feature = "norn", feature = "liminal-transport"))]
                Command::Worker(_) => anyhow::bail!("expected workflow operation command"),
                Command::Server(_)
                | Command::Dev(_)
                | Command::Client(
                    ClientCommand::Remote(
                        RemoteCommand::Start { .. }
                        | RemoteCommand::List { .. }
                        | RemoteCommand::Inspect { .. },
                    )
                    | ClientCommand::New(_)
                    | ClientCommand::Codegen { .. }
                    | ClientCommand::Generate { .. }
                    | ClientCommand::Check { .. }
                    | ClientCommand::Input { .. }
                    | ClientCommand::Package { .. }
                    | ClientCommand::Deploy { .. }
                    | ClientCommand::Versions { .. }
                    | ClientCommand::Route { .. }
                    | ClientCommand::Unload { .. },
                ) => {
                    anyhow::bail!("expected workflow operation command")
                }
            }
        }
        Ok(())
    }

    #[test]
    fn signal_cancel_and_query_accept_optional_run_id() -> anyhow::Result<()> {
        let commands = [
            vec![
                "aion",
                "signal",
                WORKFLOW_ID,
                "poke",
                "--run-id",
                RUN_ID,
                "--payload",
                "{}",
            ],
            vec!["aion", "cancel", WORKFLOW_ID, "--run-id", RUN_ID],
            vec!["aion", "reopen", WORKFLOW_ID, "--run-id", RUN_ID],
            vec!["aion", "query", WORKFLOW_ID, "state", "--run-id", RUN_ID],
        ];

        for args in commands {
            let cli = Cli::try_parse_from(args)?;
            match cli.command {
                Command::Client(ClientCommand::Remote(
                    RemoteCommand::Signal { run_id, .. }
                    | RemoteCommand::Cancel { run_id, .. }
                    | RemoteCommand::Reopen { run_id, .. }
                    | RemoteCommand::Query { run_id, .. },
                )) => assert_eq!(run_id.as_deref(), Some(RUN_ID)),
                #[cfg(all(feature = "norn", feature = "liminal-transport"))]
                Command::Worker(_) => {
                    anyhow::bail!("expected run-targeted workflow operation command")
                }
                Command::Server(_)
                | Command::Dev(_)
                | Command::Client(
                    ClientCommand::Remote(
                        RemoteCommand::Start { .. }
                        | RemoteCommand::List { .. }
                        | RemoteCommand::Describe { .. }
                        | RemoteCommand::Inspect { .. },
                    )
                    | ClientCommand::New(_)
                    | ClientCommand::Codegen { .. }
                    | ClientCommand::Generate { .. }
                    | ClientCommand::Check { .. }
                    | ClientCommand::Input { .. }
                    | ClientCommand::Package { .. }
                    | ClientCommand::Deploy { .. }
                    | ClientCommand::Versions { .. }
                    | ClientCommand::Route { .. }
                    | ClientCommand::Unload { .. },
                ) => {
                    anyhow::bail!("expected run-targeted workflow operation command")
                }
            }
        }
        Ok(())
    }

    #[test]
    fn deploy_subcommands_parse_with_global_token() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from([
            "aion",
            "--token",
            "operator-token",
            "deploy",
            "dist/order.aion",
        ])?;
        assert_eq!(cli.token.as_deref(), Some("operator-token"));
        let Command::Client(ClientCommand::Deploy { archive }) = cli.command else {
            anyhow::bail!("expected deploy command");
        };
        assert_eq!(archive, Path::new("dist/order.aion"));

        let cli = Cli::try_parse_from(["aion", "versions", "--workflow-type", "order"])?;
        assert!(cli.token.is_none());
        let Command::Client(ClientCommand::Versions { workflow_type }) = cli.command else {
            anyhow::bail!("expected versions command");
        };
        assert_eq!(workflow_type.as_deref(), Some("order"));

        let hash = "a".repeat(64);
        let cli = Cli::try_parse_from(["aion", "route", "order", &hash])?;
        let Command::Client(ClientCommand::Route {
            workflow_type,
            content_hash,
        }) = cli.command
        else {
            anyhow::bail!("expected route command");
        };
        assert_eq!(workflow_type, "order");
        assert_eq!(content_hash, hash);

        let cli = Cli::try_parse_from(["aion", "unload", "order", &hash])?;
        let Command::Client(ClientCommand::Unload {
            workflow_type,
            content_hash,
        }) = cli.command
        else {
            anyhow::bail!("expected unload command");
        };
        assert_eq!(workflow_type, "order");
        assert_eq!(content_hash, hash);
        Ok(())
    }

    #[test]
    fn deploy_help_documents_the_operator_surface() -> anyhow::Result<()> {
        let mut command = Cli::command();
        let Some(deploy) = command.find_subcommand_mut("deploy") else {
            anyhow::bail!("deploy subcommand should be registered");
        };
        let help = deploy.render_long_help().to_string();
        assert!(help.contains("operator API"));
        assert!(help.contains("deploy"));
        Ok(())
    }

    #[test]
    fn server_subcommand_converts_all_overrides() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from([
            "aion",
            "server",
            "--config",
            "dev-config.toml",
            "--listen-address",
            "127.0.0.1:18080",
            "--store-url",
            "aion.db",
            "--scheduler-threads",
            "2",
            "--drain-timeout",
            "45",
        ])?;
        let Command::Server(args) = cli.command else {
            anyhow::bail!("expected server command");
        };
        let overrides = CliOverrides::from(args);

        assert_eq!(
            overrides.config_path,
            Some(PathBuf::from("dev-config.toml"))
        );
        assert_eq!(
            overrides.listen_address.map(|address| address.port()),
            Some(18080)
        );
        assert_eq!(overrides.store_url.as_deref(), Some("aion.db"));
        assert_eq!(overrides.scheduler_threads, Some(2));
        assert_eq!(overrides.drain_timeout_seconds, Some(45));
        Ok(())
    }

    #[test]
    fn server_subcommand_maps_authoring_gleam_path() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from([
            "aion",
            "server",
            "--gleam-path",
            "/usr/local/bin/gleam",
            "--authoring-project-root",
            "/srv/aion/authoring",
        ])?;
        let Command::Server(args) = cli.command else {
            anyhow::bail!("expected server command");
        };
        let overrides = CliOverrides::from(args);

        assert_eq!(
            overrides.gleam_path,
            Some(PathBuf::from("/usr/local/bin/gleam"))
        );
        assert_eq!(
            overrides.authoring_project_root,
            Some(PathBuf::from("/srv/aion/authoring"))
        );
        Ok(())
    }

    #[test]
    fn server_workflow_package_flag_is_repeatable() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from([
            "aion",
            "server",
            "--workflow-package",
            "examples/hello-world/hello-world.aion",
            "--workflow-package",
            "local.aion",
        ])?;
        let Command::Server(args) = cli.command else {
            anyhow::bail!("expected server command");
        };
        let overrides = CliOverrides::from(args);

        assert_eq!(
            overrides.workflow_packages,
            vec![
                PathBuf::from("examples/hello-world/hello-world.aion"),
                PathBuf::from("local.aion"),
            ]
        );
        Ok(())
    }

    #[test]
    fn server_help_documents_the_server_surface() -> anyhow::Result<()> {
        let mut command = Cli::command();
        let Some(server) = command.find_subcommand_mut("server") else {
            anyhow::bail!("server subcommand should be registered");
        };
        let help = server.render_long_help().to_string();

        assert!(help.contains("Run the Aion workflow server"));
        assert!(help.contains("--config"));
        assert!(help.contains("--workflow-package"));
        Ok(())
    }

    #[test]
    fn top_level_help_lists_the_server_subcommand() -> anyhow::Result<()> {
        let error = Cli::try_parse_from(["aion", "--help"])
            .err()
            .ok_or_else(|| anyhow::anyhow!("help should exit early"))?;

        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        let help = error.to_string();
        assert!(help.contains("server"));
        assert!(help.contains("Run the Aion workflow server"));
        Ok(())
    }

    #[test]
    fn dev_parses_path_gleam_path_and_optional_debounce() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from([
            "aion",
            "dev",
            "examples/hello-world",
            "--gleam-path",
            "/usr/local/bin/gleam",
            "--debounce-ms",
            "200",
        ])?;
        let Command::Dev(args) = cli.command else {
            anyhow::bail!("expected dev command");
        };
        assert_eq!(args.path, Path::new("examples/hello-world"));
        assert_eq!(args.gleam_path, Path::new("/usr/local/bin/gleam"));
        assert_eq!(
            args.debounce_ms,
            Some(std::time::Duration::from_millis(200))
        );

        // path defaults to the current directory; debounce is optional and
        // omitted by default (no invented interval).
        let cli = Cli::try_parse_from(["aion", "dev", "--gleam-path", "gleam"])?;
        let Command::Dev(args) = cli.command else {
            anyhow::bail!("expected dev command");
        };
        assert_eq!(args.path, Path::new("."));
        assert!(args.debounce_ms.is_none());
        Ok(())
    }

    #[test]
    fn dev_requires_gleam_path_and_rejects_zero_debounce() {
        // --gleam-path is required: there is no default gleam binary (ADR-001).
        assert!(Cli::try_parse_from(["aion", "dev"]).is_err());
        // A zero debounce window is a no-op the author did not mean to request.
        assert!(
            Cli::try_parse_from(["aion", "dev", "--gleam-path", "gleam", "--debounce-ms", "0"])
                .is_err()
        );
    }

    #[test]
    fn dev_help_documents_the_instant_loop_and_pinning() -> anyhow::Result<()> {
        let mut command = Cli::command();
        let Some(dev) = command.find_subcommand_mut("dev") else {
            anyhow::bail!("dev subcommand should be registered");
        };
        let help = dev.render_long_help().to_string();

        assert!(help.contains("--gleam-path"));
        assert!(help.contains("--debounce-ms"));
        assert!(help.contains("no engine restart"));
        assert!(help.contains("in-flight"));
        Ok(())
    }

    #[test]
    fn normalize_endpoint_preserves_explicit_scheme() {
        assert_eq!(
            normalize_endpoint("https://aion.example:50051"),
            "https://aion.example:50051"
        );
    }

    #[test]
    fn normalize_endpoint_defaults_to_http_scheme() {
        assert_eq!(
            normalize_endpoint("127.0.0.1:50051"),
            "http://127.0.0.1:50051"
        );
    }
}
