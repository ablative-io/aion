//! The event-driven watch loop behind `aion dev`.
//!
//! A [`notify`] recommended watcher reports OS filesystem change events for the
//! project's `src/` tree over a channel; each relevant change drives one
//! rebuild → repackage → hot-load cycle. There is no poll interval and no
//! busy-wait: the loop blocks on the channel until the OS delivers an event
//! (ADR-001). An optional debounce window coalesces the burst of events an
//! editor emits for a single save into one rebuild.
//!
//! Rebuild failures (a type error, a packaging fault, a rejected hot-load) are
//! reported to stderr and the loop keeps watching: a broken save must never
//! tear down the dev loop, exactly as the author expects from a watch process.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvError, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{Event, RecursiveMode, Watcher};

use super::pipeline::{ReloadOutcome, rebuild_repackage_reload};
use crate::deploy::DeployTarget;

/// Gleam source extension whose changes trigger a rebuild.
const GLEAM_EXTENSION: &str = "gleam";

/// Configuration for one `aion dev` watch session.
pub struct WatchSession<'a> {
    /// Project root the watcher and pipeline operate on.
    pub project_root: &'a Path,
    /// External `gleam` binary the rebuild spawns.
    pub gleam_path: &'a Path,
    /// Running-server deploy target the hot-load pushes to.
    pub target: &'a DeployTarget,
    /// Optional debounce window applied after the first change of a burst.
    pub debounce: Option<Duration>,
}

/// Watches the project's `src/` tree and runs the reload pipeline on every
/// relevant save until the process is interrupted.
///
/// The initial cycle runs once up front so the running server holds the current
/// source before any edit, then the loop blocks on filesystem events.
///
/// # Errors
///
/// Returns an error only when the watcher cannot be installed or its event
/// channel is torn down; per-save rebuild failures are reported and the loop
/// continues.
pub async fn watch(session: &WatchSession<'_>) -> Result<()> {
    let watch_root = source_root(session.project_root);
    let (events_tx, events_rx) = channel::<notify::Result<Event>>();
    let mut watcher = notify::recommended_watcher(move |event| {
        // The watcher callback runs on notify's own thread; a closed receiver
        // only means the dev loop has already exited, so the send result is
        // intentionally not propagated here.
        let _ = events_tx.send(event);
    })
    .context("failed to create the filesystem watcher")?;
    watcher
        .watch(&watch_root, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", watch_root.display()))?;

    println!(
        "aion dev: priming {} with the current source",
        session.project_root.display()
    );
    run_cycle(session).await;

    println!(
        "aion dev: watching {} for changes (Ctrl-C to stop)",
        watch_root.display()
    );
    watch_loop(session, &events_rx).await
}

/// Blocks on filesystem events and runs one reload cycle per coalesced save.
async fn watch_loop(
    session: &WatchSession<'_>,
    events_rx: &Receiver<notify::Result<Event>>,
) -> Result<()> {
    loop {
        // Block until the OS reports a change; this is the event-driven wait,
        // never a timed poll.
        match events_rx.recv() {
            Ok(event) => {
                if !is_relevant(&event) {
                    continue;
                }
            }
            Err(RecvError) => {
                // The watcher was dropped: nothing more will ever arrive.
                return Ok(());
            }
        }

        if let Some(window) = session.debounce {
            drain_debounce_window(events_rx, window);
        }
        run_cycle(session).await;
    }
}

/// After the first change of a burst, swallows further events that arrive
/// within the debounce window so an editor's multi-event save coalesces into a
/// single rebuild. The window is measured from the first change, not extended
/// on each new event, so a steady stream of writes still rebuilds promptly.
fn drain_debounce_window(events_rx: &Receiver<notify::Result<Event>>, window: Duration) {
    let deadline = Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        match events_rx.recv_timeout(remaining) {
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        // Keep draining until the window elapses regardless of relevance: the
        // goal is purely to coalesce the burst, and the next cycle re-reads the
        // whole project anyway.
        if Instant::now() >= deadline {
            return;
        }
    }
}

/// Runs one rebuild → repackage → hot-load cycle, reporting the outcome.
///
/// A failure is logged and swallowed deliberately: a broken save must leave the
/// watch loop running so the next save can recover.
async fn run_cycle(session: &WatchSession<'_>) {
    match rebuild_repackage_reload(session.project_root, session.gleam_path, session.target).await {
        Ok(outcome) => report_reload(&outcome),
        Err(error) => {
            eprintln!("aion dev: rebuild failed: {error:#}");
        }
    }
}

/// Reports a successful hot-load to the author.
fn report_reload(outcome: &ReloadOutcome) {
    if outcome.freshly_loaded {
        let routing = if outcome.route_changed {
            "fresh runs use it; in-flight runs stay pinned"
        } else {
            "loaded but not routed; existing route still serves fresh runs"
        };
        println!(
            "aion dev: hot-loaded {} @ {} from {} ({routing})",
            outcome.workflow_type,
            outcome.content_hash,
            outcome.archive_path.display()
        );
    } else {
        println!(
            "aion dev: {} @ {} already loaded (no change)",
            outcome.workflow_type, outcome.content_hash
        );
    }
}

/// Resolves the directory the watcher subscribes to: the project's `src/` tree
/// when present, falling back to the project root so a project laid out
/// differently still triggers rebuilds.
fn source_root(project_root: &Path) -> PathBuf {
    let src = project_root.join("src");
    if src.is_dir() {
        src
    } else {
        project_root.to_path_buf()
    }
}

/// Whether a watcher event should trigger a rebuild: a create/modify/remove/
/// rename touching a `.gleam` source file. Pure access-time or metadata events,
/// and events on non-source files (build artifacts, editor swap files), are
/// ignored so the loop only rebuilds on real source edits.
fn is_relevant(event: &notify::Result<Event>) -> bool {
    let Ok(event) = event else {
        // A watcher error (for example an overflowed event queue) is treated as
        // "something changed": rebuild rather than risk missing an edit.
        return true;
    };
    if !is_content_kind(event.kind) {
        return false;
    }
    event.paths.iter().any(|path| is_gleam_source(path))
}

/// Whether an event kind represents a content change worth rebuilding for.
fn is_content_kind(kind: notify::EventKind) -> bool {
    use notify::EventKind;
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Any
    )
}

/// Whether a path is a Gleam source file by extension.
fn is_gleam_source(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(GLEAM_EXTENSION))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use notify::event::{CreateKind, EventAttributes, ModifyKind};
    use notify::{Event, EventKind};

    use super::{is_gleam_source, is_relevant, source_root};

    fn event(kind: EventKind, path: &str) -> Event {
        Event {
            kind,
            paths: vec![PathBuf::from(path)],
            attrs: EventAttributes::default(),
        }
    }

    fn modify_event(path: &str) -> Event {
        event(EventKind::Modify(ModifyKind::Any), path)
    }

    #[test]
    fn gleam_sources_are_recognised_case_insensitively() {
        assert!(is_gleam_source(Path::new("src/order.gleam")));
        assert!(is_gleam_source(Path::new("src/Order.GLEAM")));
        assert!(!is_gleam_source(Path::new("src/order.erl")));
        assert!(!is_gleam_source(Path::new("workflow.toml")));
    }

    #[test]
    fn relevant_only_for_source_content_changes() {
        assert!(is_relevant(&Ok(modify_event("src/order.gleam"))));
        assert!(is_relevant(&Ok(event(
            EventKind::Create(CreateKind::File),
            "src/new.gleam"
        ))));
        assert!(!is_relevant(&Ok(modify_event(
            "build/dev/erlang/order.beam"
        ))));
        assert!(!is_relevant(&Ok(modify_event("README.md"))));
    }

    #[test]
    fn watcher_errors_force_a_rebuild() {
        let error: notify::Result<Event> = Err(notify::Error::generic("event queue overflowed"));
        assert!(is_relevant(&error));
    }

    #[test]
    fn source_root_prefers_src_then_falls_back_to_root() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        assert_eq!(source_root(temp.path()), temp.path());
        std::fs::create_dir(temp.path().join("src"))?;
        assert_eq!(source_root(temp.path()), temp.path().join("src"));
        Ok(())
    }
}
