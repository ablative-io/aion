//! JSON output helpers for the CLI.

use std::io::{self, Write};

use aion_client::WorkflowHandle;
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
pub(crate) struct StartOutput {
    workflow_id: String,
    run_id: String,
}

#[derive(Serialize)]
pub(crate) struct AcknowledgementOutput<'a> {
    pub(crate) workflow_id: &'a str,
    pub(crate) accepted: bool,
}

#[derive(Serialize)]
pub(crate) struct QueryOutput {
    pub(crate) result: Value,
}

#[derive(Serialize)]
pub(crate) struct DescribeOutput<TSummary, THistory> {
    pub(crate) summary: TSummary,
    pub(crate) history: THistory,
}

pub(crate) fn start_output(handle: &WorkflowHandle) -> StartOutput {
    StartOutput {
        workflow_id: handle.workflow_id().to_string(),
        run_id: handle.run_id().to_string(),
    }
}

pub(crate) fn to_value<T>(value: T) -> Result<Value>
where
    T: Serialize,
{
    serde_json::to_value(value).context("failed to encode command output")
}

pub(crate) fn print_json(value: &Value, pretty: bool) -> Result<()> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    if pretty {
        serde_json::to_writer_pretty(&mut handle, value).context("failed to write JSON output")?;
    } else {
        serde_json::to_writer(&mut handle, value).context("failed to write JSON output")?;
    }
    writeln!(handle).context("failed to write trailing newline")
}
