//! Activity worker for the `{{name}}` saga workflow.
//!
//! Serves the `charge_payment` and `refund_payment` activities the workflow
//! dispatches. Input types require `Serialize + DeserializeOwned`; output
//! types require `Serialize`. Handlers classify failures explicitly with
//! `ActivityFailure::retryable(..)` or `ActivityFailure::terminal(..)` — the
//! worker never retries on its own; the classification rides back to the
//! workflow, which drives retries.

use std::time::Duration;

use aion_worker::{ActivityContext, HandlerFuture, Worker, WorkerConfig};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct ChargeInput {
    order_id: String,
    amount_cents: i64,
    attempt: u32,
}

#[derive(Serialize)]
struct PaymentReceipt {
    order_id: String,
    payment_id: String,
    amount_cents: i64,
}

#[derive(Serialize, Deserialize)]
struct RefundInput {
    order_id: String,
    payment_id: String,
    amount_cents: i64,
}

#[derive(Serialize)]
struct RefundReceipt {
    order_id: String,
    refund_id: String,
}

/// Charge the order. Replace the body with a real payment-gateway call and
/// fail with `ActivityFailure::retryable(..)` on transient gateway errors to
/// exercise the workflow's bounded retry loop.
fn charge_payment(input: ChargeInput, _context: &ActivityContext) -> HandlerFuture<'_, PaymentReceipt> {
    Box::pin(async move {
        println!(
            "charging order {} for {} cents (attempt {})",
            input.order_id, input.amount_cents, input.attempt
        );
        Ok(PaymentReceipt {
            payment_id: format!("pay-{}", input.order_id),
            order_id: input.order_id,
            amount_cents: input.amount_cents,
        })
    })
}

/// Compensate a captured payment when the saga cancels.
fn refund_payment(input: RefundInput, _context: &ActivityContext) -> HandlerFuture<'_, RefundReceipt> {
    Box::pin(async move {
        println!(
            "refunding payment {} ({} cents) for order {}",
            input.payment_id, input.amount_cents, input.order_id
        );
        Ok(RefundReceipt {
            refund_id: format!("ref-{}", input.order_id),
            order_id: input.order_id,
        })
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Every field is explicit; there are no hidden defaults. The endpoint
    // matches the gRPC address in this project's aion.toml.
    let config = WorkerConfig::builder()
        .endpoint("http://127.0.0.1:50051")
        .task_queue("default")
        .identity("{{name}}-worker-1")
        .max_concurrency(4)
        .reconnect_initial_backoff(Duration::from_millis(100))
        .reconnect_max_backoff(Duration::from_secs(5))
        .reconnect_max_attempts(10)
        .build()?;

    Worker::builder(config)
        .register_activity("charge_payment", charge_payment)?
        .register_activity("refund_payment", refund_payment)?
        .build()?
        .run()
        .await?;

    Ok(())
}
