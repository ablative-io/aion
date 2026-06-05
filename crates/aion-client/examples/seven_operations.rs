//! Runnable aion-client example covering connect plus all seven workflow operations.

use std::env;
use std::time::Duration;

use aion_client::{ClientAuth, ClientBuilder, ClientError, ListPage, StartOptions};
use aion_core::WorkflowFilter;
use futures::StreamExt;
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
struct StartInput<'a> {
    message: &'a str,
    counter: u32,
}

#[derive(Serialize)]
struct SignalInput<'a> {
    value: &'a str,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        match error {
            ClientError::Unavailable => {
                eprintln!("aion-server is unavailable; check AION_SERVER_URL and the fixture");
            }
            ClientError::AlreadyExists => {
                eprintln!("idempotency key was reused for a different start request");
            }
            ClientError::QueryTimeout => eprintln!("query timed out before the fixture replied"),
            other => eprintln!("aion-client example failed: {other}"),
        }
        std::process::exit(1);
    }
}

async fn run() -> Result<(), ClientError> {
    let endpoint = env::args()
        .nth(1)
        .or_else(|| env::var("AION_SERVER_URL").ok())
        .unwrap_or_else(|| String::from("http://127.0.0.1:50051"));
    let mut builder = ClientBuilder::new(endpoint).with_namespace("conformance");
    if let Ok(token) = env::var("AION_AUTH_TOKEN") {
        builder = builder.with_auth(ClientAuth::bearer(token));
    }
    let client = builder.build().await?;

    let handle = client
        .start_typed(
            "conformance.echo",
            &StartInput {
                message: "hello",
                counter: 1,
            },
            StartOptions {
                idempotency_key: Some(format!(
                    "aion-client-rust-seven-operations-{}",
                    uuid::Uuid::new_v4()
                )),
                ..StartOptions::default()
            },
        )
        .await?;
    println!(
        "started workflow={} run={}",
        handle.workflow_id(),
        handle.run_id()
    );

    handle
        .signal_typed(
            "record",
            &SignalInput {
                value: "signal-observed",
            },
        )
        .await?;
    println!("sent signal record");

    let state: Value = handle
        .query_typed::<(), Value>("state", &(), Duration::from_secs(5))
        .await?;
    println!("query state={state}");

    let summaries = client
        .list(
            &WorkflowFilter {
                workflow_type: Some(String::from("conformance.echo")),
                ..WorkflowFilter::default()
            },
            ListPage::default(),
        )
        .await?;
    println!("listed {} workflow(s)", summaries.len());

    let description = handle.describe().await?;
    println!("described {} event(s)", description.history.len());

    handle
        .cancel("seven-operations example requested cancellation")
        .await?;
    println!("cancel requested");

    let mut events = handle.subscribe();
    match tokio::time::timeout(Duration::from_secs(5), events.next()).await {
        Ok(Some(Ok(event))) => println!("subscribed event seq={}", event.seq()),
        Ok(Some(Err(error))) => return Err(error),
        Ok(None) => println!("subscription ended without events"),
        Err(_) => return Err(ClientError::QueryTimeout),
    }

    Ok(())
}
