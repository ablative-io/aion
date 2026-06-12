//! Command-line workflow operations for Aion.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use aion_client::{Client, ClientBuilder, ListPage, StartOptions};
use aion_core::{WorkflowFilter, WorkflowStatus};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;

use crate::output::{
    AcknowledgementOutput, QueryOutput, describe_output, print_json, start_output, to_value,
};
use crate::payload::{
    empty_query_payload, json_payload, parse_run_id, parse_status, parse_workflow_id,
    payload_to_json,
};

mod deploy;
mod output;
mod package;
mod payload;
mod render;

const DEFAULT_ENDPOINT: &str = "127.0.0.1:50051";
const DEFAULT_NAMESPACE: &str = "default";
const DEFAULT_SUBJECT: &str = "cli-user";
const QUERY_DEADLINE: Duration = Duration::from_secs(5);

/// Aion workflow command-line client.
#[derive(Debug, Parser)]
#[command(name = "aion-cli", version, about = "Operate Aion workflows over gRPC")]
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
        #[arg(long, default_value = "cancelled by aion-cli")]
        reason: String,
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
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = run(&cli)
        .await
        .and_then(|value| print_json(&value, cli.pretty));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        // The single rendering contract for every subcommand: the taxonomy
        // class, server detail, error type, and hint go to stderr; the
        // process exits 1.
        Err(error) => {
            eprintln!("{}", render::render_error(&error));
            ExitCode::FAILURE
        }
    }
}

/// Dispatches the parsed command, connecting to the server only for remote
/// commands. `package` is local-only and never constructs a client; the
/// deploy subcommands drive the operator `DeployService` directly and never
/// touch the caller SDK.
async fn run(cli: &Cli) -> Result<Value> {
    match &cli.command {
        Command::Package { path, out, build } => package::run(path, out.as_deref(), *build),
        Command::Deploy { archive } => deploy::deploy(&deploy_target(cli), archive).await,
        Command::Versions { workflow_type } => {
            deploy::versions(&deploy_target(cli), workflow_type.as_deref()).await
        }
        Command::Route {
            workflow_type,
            content_hash,
        } => deploy::route(&deploy_target(cli), workflow_type, content_hash).await,
        Command::Unload {
            workflow_type,
            content_hash,
        } => deploy::unload(&deploy_target(cli), workflow_type, content_hash).await,
        Command::Remote(command) => {
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
        RemoteCommand::List { status } => list_workflows(client, *status).await,
        RemoteCommand::Describe {
            workflow_id,
            run_id,
            raw,
        } => describe_workflow(client, workflow_id, run_id.as_deref(), *raw).await,
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
    use std::path::Path;

    use clap::{CommandFactory, Parser};

    use super::{Cli, Command, RemoteCommand, normalize_endpoint};

    const WORKFLOW_ID: &str = "00000000-0000-0000-0000-000000000001";
    const RUN_ID: &str = "00000000-0000-0000-0000-000000000002";

    #[test]
    fn describe_accepts_optional_run_id() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from(["aion-cli", "describe", WORKFLOW_ID, "--run-id", RUN_ID])?;

        let Command::Remote(RemoteCommand::Describe {
            workflow_id,
            run_id,
            raw,
        }) = cli.command
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
        let cli = Cli::try_parse_from(["aion-cli", "describe", WORKFLOW_ID, "--raw"])?;

        let Command::Remote(RemoteCommand::Describe { raw, .. }) = cli.command else {
            anyhow::bail!("expected describe command");
        };
        assert!(raw);
        Ok(())
    }

    #[test]
    fn package_defaults_to_current_directory_without_build_or_out() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from(["aion-cli", "package"])?;

        let Command::Package { path, out, build } = cli.command else {
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
            "aion-cli",
            "package",
            "examples/hello-world",
            "--out",
            "dist/hello.aion",
            "--build",
        ])?;

        let Command::Package { path, out, build } = cli.command else {
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
            vec!["aion-cli", "describe", WORKFLOW_ID],
            vec!["aion-cli", "signal", WORKFLOW_ID, "poke", "--payload", "{}"],
            vec!["aion-cli", "cancel", WORKFLOW_ID],
            vec!["aion-cli", "query", WORKFLOW_ID, "state"],
        ];

        for args in commands {
            let cli = Cli::try_parse_from(args)?;
            match cli.command {
                Command::Remote(
                    RemoteCommand::Describe { run_id, .. }
                    | RemoteCommand::Signal { run_id, .. }
                    | RemoteCommand::Cancel { run_id, .. }
                    | RemoteCommand::Query { run_id, .. },
                ) => assert!(run_id.is_none()),
                Command::Remote(RemoteCommand::Start { .. } | RemoteCommand::List { .. })
                | Command::Package { .. }
                | Command::Deploy { .. }
                | Command::Versions { .. }
                | Command::Route { .. }
                | Command::Unload { .. } => {
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
                "aion-cli",
                "signal",
                WORKFLOW_ID,
                "poke",
                "--run-id",
                RUN_ID,
                "--payload",
                "{}",
            ],
            vec!["aion-cli", "cancel", WORKFLOW_ID, "--run-id", RUN_ID],
            vec![
                "aion-cli",
                "query",
                WORKFLOW_ID,
                "state",
                "--run-id",
                RUN_ID,
            ],
        ];

        for args in commands {
            let cli = Cli::try_parse_from(args)?;
            match cli.command {
                Command::Remote(
                    RemoteCommand::Signal { run_id, .. }
                    | RemoteCommand::Cancel { run_id, .. }
                    | RemoteCommand::Query { run_id, .. },
                ) => assert_eq!(run_id.as_deref(), Some(RUN_ID)),
                Command::Remote(
                    RemoteCommand::Start { .. }
                    | RemoteCommand::List { .. }
                    | RemoteCommand::Describe { .. },
                )
                | Command::Package { .. }
                | Command::Deploy { .. }
                | Command::Versions { .. }
                | Command::Route { .. }
                | Command::Unload { .. } => {
                    anyhow::bail!("expected run-targeted workflow operation command")
                }
            }
        }
        Ok(())
    }

    #[test]
    fn deploy_subcommands_parse_with_global_token() -> anyhow::Result<()> {
        let cli = Cli::try_parse_from([
            "aion-cli",
            "--token",
            "operator-token",
            "deploy",
            "dist/order.aion",
        ])?;
        assert_eq!(cli.token.as_deref(), Some("operator-token"));
        let Command::Deploy { archive } = cli.command else {
            anyhow::bail!("expected deploy command");
        };
        assert_eq!(archive, Path::new("dist/order.aion"));

        let cli = Cli::try_parse_from(["aion-cli", "versions", "--workflow-type", "order"])?;
        assert!(cli.token.is_none());
        let Command::Versions { workflow_type } = cli.command else {
            anyhow::bail!("expected versions command");
        };
        assert_eq!(workflow_type.as_deref(), Some("order"));

        let hash = "a".repeat(64);
        let cli = Cli::try_parse_from(["aion-cli", "route", "order", &hash])?;
        let Command::Route {
            workflow_type,
            content_hash,
        } = cli.command
        else {
            anyhow::bail!("expected route command");
        };
        assert_eq!(workflow_type, "order");
        assert_eq!(content_hash, hash);

        let cli = Cli::try_parse_from(["aion-cli", "unload", "order", &hash])?;
        let Command::Unload {
            workflow_type,
            content_hash,
        } = cli.command
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
