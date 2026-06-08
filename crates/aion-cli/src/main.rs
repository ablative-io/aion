//! Command-line workflow operations for Aion.

use std::time::Duration;

use aion_client::{Client, ClientBuilder, ListPage, StartOptions};
use aion_core::{WorkflowFilter, WorkflowStatus};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;

use crate::output::{
    AcknowledgementOutput, DescribeOutput, QueryOutput, print_json, start_output, to_value,
};
use crate::payload::{
    empty_query_payload, json_payload, parse_run_id, parse_status, parse_workflow_id,
    payload_to_json,
};

mod output;
mod payload;

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
    /// Print formatted, human-readable JSON.
    #[arg(long, global = true)]
    pretty: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
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
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = build_client(&cli).await?;
    let value = execute(&client, &cli.command).await?;
    print_json(&value, cli.pretty)?;
    Ok(())
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

async fn execute(client: &Client, command: &Command) -> Result<Value> {
    match command {
        Command::Start {
            workflow_type,
            input,
        } => start_workflow(client, workflow_type, input).await,
        Command::Signal {
            workflow_id,
            signal_name,
            run_id,
            payload,
        } => signal_workflow(client, workflow_id, signal_name, run_id.as_deref(), payload).await,
        Command::Query {
            workflow_id,
            query_name,
            run_id,
        } => query_workflow(client, workflow_id, query_name, run_id.as_deref()).await,
        Command::Cancel {
            workflow_id,
            reason,
            run_id,
        } => cancel_workflow(client, workflow_id, reason, run_id.as_deref()).await,
        Command::List { status } => list_workflows(client, *status).await,
        Command::Describe {
            workflow_id,
            run_id,
        } => describe_workflow(client, workflow_id, run_id.as_deref()).await,
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
) -> Result<Value> {
    let workflow_id = parse_workflow_id(workflow_id)?;
    let run_id = run_id.map(parse_run_id).transpose()?;
    let description = client
        .describe(&workflow_id, run_id.as_ref())
        .await
        .context("failed to describe workflow")?;
    to_value(DescribeOutput {
        summary: description.summary,
        history: description.history,
    })
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
    use clap::Parser;

    use super::{Cli, Command, normalize_endpoint};

    const WORKFLOW_ID: &str = "00000000-0000-0000-0000-000000000001";
    const RUN_ID: &str = "00000000-0000-0000-0000-000000000002";

    #[test]
    fn describe_accepts_optional_run_id() {
        let cli = Cli::try_parse_from(["aion-cli", "describe", WORKFLOW_ID, "--run-id", RUN_ID])
            .expect("describe with --run-id should parse");

        let Command::Describe {
            workflow_id,
            run_id,
        } = cli.command
        else {
            panic!("expected describe command");
        };
        assert_eq!(workflow_id, WORKFLOW_ID);
        assert_eq!(run_id.as_deref(), Some(RUN_ID));
    }

    #[test]
    fn workflow_operations_allow_omitted_run_id() {
        let commands = [
            vec!["aion-cli", "describe", WORKFLOW_ID],
            vec!["aion-cli", "signal", WORKFLOW_ID, "poke", "--payload", "{}"],
            vec!["aion-cli", "cancel", WORKFLOW_ID],
            vec!["aion-cli", "query", WORKFLOW_ID, "state"],
        ];

        for args in commands {
            let cli = Cli::try_parse_from(args).expect("command without --run-id should parse");
            match cli.command {
                Command::Describe { run_id, .. }
                | Command::Signal { run_id, .. }
                | Command::Cancel { run_id, .. }
                | Command::Query { run_id, .. } => assert!(run_id.is_none()),
                Command::Start { .. } | Command::List { .. } => {
                    panic!("expected workflow operation command")
                }
            }
        }
    }

    #[test]
    fn signal_cancel_and_query_accept_optional_run_id() {
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
            let cli = Cli::try_parse_from(args).expect("command with --run-id should parse");
            match cli.command {
                Command::Signal { run_id, .. }
                | Command::Cancel { run_id, .. }
                | Command::Query { run_id, .. } => assert_eq!(run_id.as_deref(), Some(RUN_ID)),
                Command::Start { .. } | Command::List { .. } | Command::Describe { .. } => {
                    panic!("expected run-targeted workflow operation command")
                }
            }
        }
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
