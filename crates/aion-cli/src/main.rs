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
    empty_query_payload, json_payload, parse_status, parse_workflow_id, payload_to_json,
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
    },
    /// Request workflow cancellation.
    Cancel {
        /// Workflow identifier.
        workflow_id: String,
        /// Cancellation reason.
        #[arg(long, default_value = "cancelled by aion-cli")]
        reason: String,
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
            payload,
        } => signal_workflow(client, workflow_id, signal_name, payload).await,
        Command::Query {
            workflow_id,
            query_name,
        } => query_workflow(client, workflow_id, query_name).await,
        Command::Cancel {
            workflow_id,
            reason,
        } => cancel_workflow(client, workflow_id, reason).await,
        Command::List { status } => list_workflows(client, *status).await,
        Command::Describe { workflow_id } => describe_workflow(client, workflow_id).await,
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
    payload: &str,
) -> Result<Value> {
    let workflow_id = parse_workflow_id(workflow_id)?;
    let payload = json_payload(payload).context("invalid --payload JSON")?;
    client
        .signal(&workflow_id, None, signal_name.to_owned(), payload)
        .await
        .context("failed to signal workflow")?;
    to_value(AcknowledgementOutput {
        workflow_id: &workflow_id.to_string(),
        accepted: true,
    })
}

async fn query_workflow(client: &Client, workflow_id: &str, query_name: &str) -> Result<Value> {
    let workflow_id = parse_workflow_id(workflow_id)?;
    let payload = client
        .query(
            &workflow_id,
            None,
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

async fn cancel_workflow(client: &Client, workflow_id: &str, reason: &str) -> Result<Value> {
    let workflow_id = parse_workflow_id(workflow_id)?;
    client
        .cancel(&workflow_id, None, reason.to_owned())
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

async fn describe_workflow(client: &Client, workflow_id: &str) -> Result<Value> {
    let workflow_id = parse_workflow_id(workflow_id)?;
    let description = client
        .describe(&workflow_id, None)
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
    use super::normalize_endpoint;

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
