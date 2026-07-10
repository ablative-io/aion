//! Composition root for the awl-hello worker.
//!
//! It serves TWO pure computational activities (`greet`, `shout`) on ONE
//! task queue (`awl_hello`) from a SINGLE liminal connection registered on
//! node `hello` — the smallest instance of the routing model the dev-brief
//! worker exercises in full: the server routes a pushed activity by
//! (namespace, `task_queue`, node) ONLY, never by activity type, and this
//! worker's one connection holds both handlers. The rev-2 `awl_hello.awl`
//! declares its actions with no `node` config, so its dispatches are
//! node-unpinned and reach this worker by queue alone.
//!
//! No agents, no harness, no shelling out: the handler bodies are pure
//! functions over their typed inputs, adapted onto the SDK's async handler
//! signature here.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use aion_worker::{
    ActivityContext, ActivityFailure, ActivityRegistry, HandlerFuture, RedialTiming, WorkerConfig,
    WorkerConfigBuildError,
};

use awl_hello_worker::activities;

/// The default liminal listen address the worker dials — the server's
/// `[outbox] liminal_listen_address`.
const DEFAULT_ADDRESS: &str = "127.0.0.1:50061";
/// The one task queue every awl-hello activity is dispatched on.
const TASK_QUEUE: &str = "awl_hello";
/// The node id this worker's single connection registers. The rev-2
/// workflow's dispatches are node-unpinned (no `node` config on its
/// actions), and an unpinned dispatch matches every worker on the queue —
/// so this registration is locality metadata here. A workflow that DOES pin
/// (`node hello` on an action's config line) must match this string exactly:
/// the server matches it blindly, and a drift on either side strands
/// activities on handlerless connections.
const NODE: &str = "hello";
const REDIAL_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const REDIAL_MAX_BACKOFF: Duration = Duration::from_secs(5);

/// Parsed command-line arguments.
#[derive(Debug)]
struct Args {
    candidates: Vec<String>,
    identity: String,
    ready_file: Option<String>,
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
        task_queue = TASK_QUEUE,
        node = NODE,
        "awl-hello-worker starting: 2 computational activities on 1 connection"
    );

    let config = worker_config(&args.identity)?;
    let registry = Arc::new(registry()?);
    let stop = AtomicBool::new(false);
    let ready_file = args.ready_file.clone();
    aion_worker::serve_with_redial(
        args.candidates.clone(),
        &config,
        &registry,
        RedialTiming::new(REDIAL_INITIAL_BACKOFF, REDIAL_MAX_BACKOFF),
        &stop,
        None,
        move || {
            if let Some(path) = ready_file.as_ref()
                && let Err(error) = std::fs::write(path, b"connected")
            {
                tracing::error!(%error, "failed to write worker readiness file");
            }
            tracing::info!("connection registered; serving greet/shout");
        },
    )?;
    Ok(())
}

/// The two activities as a typed registry — the ONE definition of what this
/// worker's connection (node [`NODE`]) serves.
fn registry() -> Result<ActivityRegistry, aion_worker::WorkerError> {
    ActivityRegistry::new()
        .register_activity("greet", pure(activities::greet))?
        .register_activity("shout", pure(activities::shout))
}

/// The worker config for the one connection: one identity, the awl-hello
/// task queue, the [`NODE`] routing key, and an effectively unbounded
/// reconnect budget (a long-lived worker must outwait server restarts).
fn worker_config(identity: &str) -> Result<WorkerConfig, WorkerConfigBuildError> {
    WorkerConfig::builder()
        .endpoint("unused-direct-address")
        .namespace("default")
        .task_queue(TASK_QUEUE)
        .node(NODE)
        .identity(identity)
        .max_concurrency(4)
        .reconnect_initial_backoff(REDIAL_INITIAL_BACKOFF)
        .reconnect_max_backoff(REDIAL_MAX_BACKOFF)
        .reconnect_max_attempts(usize::MAX)
        .build()
}

/// Adapt a pure, synchronous handler body onto the worker SDK's async
/// handler signature — the bodies are instant string work, so no blocking
/// pool is needed.
fn pure<Input, Output>(
    body: fn(Input) -> Result<Output, ActivityFailure>,
) -> impl for<'context> Fn(Input, &'context ActivityContext) -> HandlerFuture<'context, Output>
+ Send
+ Sync
+ 'static
where
    Input: Send + 'static,
    Output: Send + 'static,
{
    move |input: Input, _context: &ActivityContext| Box::pin(async move { body(input) })
}

fn parse_args() -> anyhow::Result<Args> {
    parse_args_from(std::env::args().skip(1))
}

/// The argument-parsing core, fed an explicit iterator so tests exercise the
/// exact production logic without touching process globals.
fn parse_args_from(args: impl IntoIterator<Item = String>) -> anyhow::Result<Args> {
    let mut candidates: Vec<String> = Vec::new();
    let mut identity = "awl-hello-worker".to_owned();
    let mut ready_file: Option<String> = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => candidates.push(next_value(&mut args, "--address")?),
            "--identity" => identity = next_value(&mut args, "--identity")?,
            "--ready-file" => ready_file = Some(next_value(&mut args, "--ready-file")?),
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
    })
}

/// Take the value for a value-taking flag, bailing clearly when it is
/// missing — a silent default would mask an operator typo.
fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<String> {
    args.next()
        .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_ADDRESS, parse_args_from, registry};

    fn parse(args: &[&str]) -> anyhow::Result<super::Args> {
        parse_args_from(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn no_arguments_yield_the_defaults() -> anyhow::Result<()> {
        let args = parse(&[])?;
        assert_eq!(args.candidates, vec![DEFAULT_ADDRESS.to_owned()]);
        assert_eq!(args.identity, "awl-hello-worker");
        assert_eq!(args.ready_file, None);
        Ok(())
    }

    #[test]
    fn every_value_taking_flag_bails_when_missing() {
        for flag in ["--address", "--identity", "--ready-file"] {
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
            Some("unknown argument `--bogus`".to_owned()),
        );
    }

    /// The served activity-type set — the workflow side (placeholder AND
    /// generated module) must dispatch exactly these on node `hello`.
    #[test]
    fn registry_serves_exactly_greet_and_shout() -> Result<(), aion_worker::WorkerError> {
        let registry = registry()?;
        let activity_types: Vec<String> = registry.activity_types().into_iter().collect();
        assert_eq!(activity_types, vec!["greet", "shout"]);
        Ok(())
    }
}
