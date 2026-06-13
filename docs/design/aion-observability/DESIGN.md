---
type: design
cluster: aion-observability
title: Loud Workflow-Process Failures: a crash never looks like a hang
---

# Loud Workflow-Process Failures: a crash never looks like a hang

> **Cluster:** aion-observability

## Intention

When a workflow process crashes, the operator watching the server learns it at the moment it happens — an error-level log line carrying the workflow id, the workflow type, and the underlying VM message — and a child's terminal failure surfaces to its parent run and to anyone watching the event stream without a manual investigation. When this is done, the only way a failure can be discovered is no longer `aion describe`: the failure is loud where people already look (the server log, the worker log, the parent's timeline, the watcher stream), and silence on those surfaces means the run is genuinely still alive, not dead-and-mute. The durable record stays the source of truth; observability is the speaker that reads it aloud the instant the terminal lands.

## Problem

During the brief_dev real-norn dogfood (2026-06-13) the inner workflow process crashed with a beamr heap-full right after the scout stage. The engine did exactly what durability demands: it recorded a terminal `WorkflowFailed` event through the handle's Recorder. But it logged NOTHING — not in the server log, not in the worker log — at the moment the process exited. The exit funnel in `crates/aion/src/lifecycle/completion.rs` (`handle_process_exit_async`) records the failure and notifies waiters but emits no tracing call on the abnormal-exit path; the server-side `InstrumentedEventStore` sees the `WorkflowFailed` event only to bump a metrics counter, never a log line; the monitor wiring in `lifecycle/start.rs` and `engine/startup.rs` logs only if the recording itself errors, never the exit. With no engine-imposed timeout (ADR-003) the run had no deadline to trip, so to the operator it looked exactly like a hang — the only way to discover the failure was a manual `aion describe`. Separately, when a child workflow fails its terminal is recorded onto the parent's history by the child-terminal watcher (`runtime/nif_child_watch.rs`), but that normal record path logs nothing either, and a dashboard/watcher consuming the live event stream has no eyes on it beyond the raw event itself. The crash class is fixed (beamr 0.6.1); the observability gap that let a crash masquerade as a hang is what this cluster closes.

## Solution

Two disjoint, additive changes — logging and propagation/visibility over the recorded terminal events, no new mutable state and (preferably) no new event types. (1) Loud exit logging (OBS-001): `handle_process_exit_async` in `crates/aion/src/lifecycle/completion.rs` is the single funnel every workflow-process exit passes through — both freshly started runs (wired via `lifecycle/start.rs::install_completion_monitor`) and recovered runs (`engine/startup.rs::install_recovered_completion_monitor`) route their monitor callbacks here. Adding the log at this one funnel covers every exit path with no path left silent (CLAUDE.md 'No silent failures'): a clean completion logs at info, an abnormal/failed exit and the monitor-failure branch log at error with workflow id, run id, workflow type, and the `WorkflowError` message (the VM/exit reason text, formatted in `runtime/outcome.rs`). The idempotent-replay branch (a terminal already in history) does not re-log, so recovery never double-shouts a failure that was already announced. On the server surface, the `InstrumentedEventStore` (`crates/aion-server/src/observability/instrumented_store.rs`) already inspects every appended event for metrics; it gains an error-level log on `WorkflowFailed`/`WorkflowTimedOut` and on `ChildWorkflowFailed` carrying the namespace, workflow id, and the recorded `WorkflowError` message — so an operator tailing the server log sees the failure even when the failing process is on a remote worker the server never hosted. The worker (`crates/aion-worker`) already logs activity-level decode and panic failures at error; OBS-001 confirms and, where a worker is the first surface to observe a relevant workflow-process failure, makes that explicit — the worker is not the host of the workflow process (that is the engine), so its surface stays activity-scoped and OBS-001 does not invent a workflow-failure log there it cannot truthfully emit. (2) Visible child propagation (OBS-002): the failed-child terminal is already recorded onto the parent's history as `ChildWorkflowFailed` by `record_parent_child_terminal` in `runtime/nif_child_watch.rs`; this is what makes the parent's `describe`/timeline already show the child failed and its reason — OBS-002 verifies that path end to end (the recorded `ChildWorkflowFailed` carries the child's reason and is encoded into the describe history by `aion-server/src/api/handlers/describe.rs` → `payload.rs::encode_history`) and adds the loud edge: an error-level log when the watcher records a `ChildWorkflowFailed` (naming parent workflow id, child workflow id, reason), matching the warn/error lines the watcher already emits for transient and invariant record failures. The watcher seam is also where the server's live event stream (`aion-server/src/stream/`) is fed: because the parent-side `ChildWorkflowFailed` is a normal appended event, the existing subscription/websocket push surface already forwards it to watchers — OBS-002 verifies a subscribed watcher receives the parent-side child-failure event, so the dashboard/notification seam reflects it without a new push path. Status remains a projection throughout (invariant 4, CN1): every log line and every propagation reads a terminal event that already exists; nothing introduces a status field or a second source of truth.

## Principles

- **P1** — Read the terminal, never store it: every log line and every propagation is derived from an already-recorded terminal event (WorkflowFailed, ChildWorkflowFailed) — observability never writes status and never becomes a second source of truth (invariant 4).
- **P2** — Loud at the moment, not only in the ledger: the durable event is the record; the log line is the announcement, emitted at the instant the terminal is decided so an operator learns it without reading event history.
- **P3** — Every exit path through one funnel: workflow-process exits are logged at the single funnel they all pass through (handle_process_exit_async), so 'cover every path' is structural, not a list of sites that can drift out of sync.
- **P4** — Log the failure once: the idempotent-replay branch (a terminal already in history) does not re-emit the failure log — recovery reconciles silently, only the first observation shouts.
- **P5** — Logging is additive and side-effect-free on control flow: adding a tracing call never changes what is recorded, notified, reconciled, or returned — a failure to log is itself never allowed to suppress the durable terminal.

## Decisions

- ADR-003 — No default timeouts anywhere in the engine — The engine imposes no activity time bound of its own. Activity waits are unbounded, terminated only by completion, worker loss, server shutdown, or a workflow-level timeout the author explicitly chose. Rejected: a bigger default — agentic activities legitimately run for over an hour, and any number we picked would be ADR-001 violated.
- ADR-005 — Failed runs are terminal; recovery resumes Running runs only — A failed run is a terminal, immutable record. Retry means a fresh `aion start` with a new run identity; recovery after restart resumes Running runs only. Rejected: in-place retry of failed runs — it would rewrite history that event-sourcing exists to preserve, and 'which attempt was this event from' becomes unanswerable.

## Goals

- When a workflow process exits abnormally, an error-level log line containing the workflow id, workflow type, and the underlying VM/WorkflowError message is emitted by the engine at the moment the exit is observed — verified by a unit test asserting the log on the abnormal-exit branch of handle_process_exit_async.
- Every workflow-process exit path (freshly started and recovered) routes through the one logging funnel; a clean completion logs at info and a failure at error, and no exit path reaches a terminal record without a corresponding log — verified by inspection that start.rs and startup.rs monitors both call handle_process_exit and by tests on the funnel's branches.
- An operator tailing the server log sees an error-level line when a WorkflowFailed, WorkflowTimedOut, or ChildWorkflowFailed event is appended, carrying namespace, workflow id, and the recorded error message — verified by a unit test on InstrumentedEventStore::record_events.
- A child workflow's terminal failure surfaces on the parent's describe timeline with the child's id and reason, and is logged at error by the child-terminal watcher when the parent-side ChildWorkflowFailed is recorded — verified by a test asserting both the recorded event content and the log.
- A watcher subscribed to the event stream receives the parent-side ChildWorkflowFailed event when a child fails — verified by an existing-stream test asserting the event reaches a subscription.
- No mutable status field, no new durable event type, and no #[allow]/unwrap/expect in library code are introduced; clippy -D warnings stays clean and every file remains under 500 LOC.

## Non-Goals

- Introducing an engine-imposed default timeout so a hang trips a deadline. — ADR-003 forbids engine-imposed time bounds; the fix is to make a crash loud, not to convert silence into a timeout. A genuine hang (no crash) is out of scope for this cluster.
- In-place retry or resurrection of a failed run after the loud log. — ADR-005: failed runs are terminal and immutable; this cluster announces the failure, it does not change failure semantics.
- A new durable event type for 'failure observed' or 'failure announced'. — The terminal WorkflowFailed/ChildWorkflowFailed events already carry the truth; a second event would duplicate the record and risk a divergent source (invariant 1, invariant 3). A new event is only justified if propagation cannot be expressed over existing events — it can.
- A new dashboard push channel, SSE endpoint, or notification integration. — The parent-side ChildWorkflowFailed is a normal appended event that the existing subscription/websocket stream already forwards; OBS-002 verifies that path rather than building a parallel one.
- Changing the WorkflowError message format or the VM exit-reason formatting in runtime/outcome.rs. — The message text is the contract this cluster surfaces verbatim; reshaping it is a separate concern and would churn the dogfood post-mortem evidence.
- Coordinating with RM-022/RM-023. — This cluster is Rust in aion-server, the aion engine, and aion-worker — disjoint file sets from RM-022/023, so it runs concurrently with them as part of the parallel stress batch (RM-024 notes).

## Structure

| Path | Note | Brief |
|------|------|-------|
| `crates/aion/src/lifecycle/completion.rs` | The single workflow-process-exit funnel (handle_process_exit_async): gains the info/error exit log on the abnormal, monitor-failure, and clean branches; replay branch stays quiet | OBS-001 |
| `crates/aion/src/lifecycle/start.rs` | install_completion_monitor for freshly started runs — confirmed to route its callback through handle_process_exit (no new log site added here) |  |
| `crates/aion/src/engine/startup.rs` | install_recovered_completion_monitor for recovered runs — confirmed to route through the same funnel |  |
| `crates/aion/src/runtime/outcome.rs` | Where the beamr ExitReason is converted into the WorkflowError message that the log surfaces verbatim; read-only reference, not modified |  |
| `crates/aion-server/src/observability/instrumented_store.rs` | record_events gains error-level logs on WorkflowFailed/WorkflowTimedOut/ChildWorkflowFailed alongside the existing metrics counters | OBS-001 |
| `crates/aion/src/runtime/nif_child_watch.rs` | record_parent_child_terminal gains an error-level log when it records a ChildWorkflowFailed (the failed-child propagation), matching its existing transient/invariant log lines | OBS-002 |
| `crates/aion-server/src/api/handlers/describe.rs` | describe handler encodes the parent history including ChildWorkflowFailed — read-only reference for OBS-002's timeline verification |  |
| `crates/aion-server/src/api/handlers/payload.rs` | encode_history turns the recorded ChildWorkflowFailed into the wire timeline describe returns — read-only reference |  |
| `crates/aion-server/src/stream/subscribe.rs` | Event subscription that forwards appended events (including parent-side ChildWorkflowFailed) to watchers — read-only reference for OBS-002's watcher verification |  |
| `crates/aion-core/src/status.rs` | status_from_events: the projection that proves status is derived, never stored — the invariant-4 anchor both briefs must not violate |  |

## Inventory

- `crates/aion/src/lifecycle/completion.rs` — handle_process_exit_async records WorkflowCompleted/WorkflowFailed under the recorder lock, notifies waiters, reconciles registry. NO tracing call on any exit branch today — the silent path the dogfood hit. The abnormal-exit and monitor-failure branches build/record a WorkflowError and return Ok without logging.
- `crates/aion/src/lifecycle/start.rs` — install_completion_monitor wires the runtime monitor for new runs to handle_process_exit; logs ONLY if handle_process_exit itself errors (line ~233), never the exit it processed.
- `crates/aion/src/engine/startup.rs` — install_recovered_completion_monitor mirrors start.rs for recovered runs; same log-only-on-recording-error pattern (line ~291).
- `crates/aion-server/src/observability/instrumented_store.rs` — InstrumentedEventStore::record_events matches WorkflowFailed/Cancelled/TimedOut/etc. to bump metrics counters only — no log line on any terminal event.
- `crates/aion/src/runtime/nif_child_watch.rs` — record_parent_child_terminal records ChildWorkflowCompleted/ChildWorkflowFailed onto the parent under the parent recorder lock. The watcher already logs warn on transient record failure and error on invariant violation and on undeliverable wake marker — but the successful ChildWorkflowFailed record itself logs nothing.
- `crates/aion-server/src/observability/tracing.rs` — Server tracing init: JSON subscriber, AION_LOG/RUST_LOG, default info. The level conventions (workflow_id via %, error via %) OBS-001/002 match.
- `crates/aion-server/src/stream/` — Subscription + websocket push surface; forwards appended events to watchers through a namespace gate. Already carries ChildWorkflowFailed because it is a normal event — no new push path needed.
- `crates/aion-worker/src/activity.rs` — Worker logs activity input-decode failures and handler panics at error with structured fields; it serves activities, it does not host the workflow process, so it has no workflow-process-exit it could log.

## Constraints

- **CN1** — Status stays a projection (invariant 4): every log and every propagation READS an already-recorded terminal event; this cluster introduces no mutable status field and no second source of truth, and never calls status_from_events to gate a write.
- **CN2** — No silent failures (CLAUDE.md): every workflow-process exit path logs. Because all paths funnel through handle_process_exit_async, the abnormal-exit branch, the monitor-failure branch, and the clean-completion branch each emit a log; the only branch that does not is the idempotent replay of an already-recorded terminal (P4).
- **CN3** — Logging over new events: OBS-001 and OBS-002 add no new durable Event variant and no new Payload type; they log existing terminals and rely on the existing recorded ChildWorkflowFailed for propagation (invariant 1's type-erased Event/Payload contract is untouched).
- **CN4** — Single-writer Recorder respected (invariant 3): no brief calls EventStore::append directly; the ChildWorkflowFailed OBS-002 surfaces is the one already written by record_parent_child_terminal through the parent's Recorder under its lock — no second writer is added.
- **CN5** — Adding a log never alters control flow: the tracing calls are pure side effects emitted before the existing notify/reconcile/return; no recorded event, no notification, no registry reconciliation, and no return value changes, and a log macro can never short-circuit a durable terminal (P5).
- **CN6** — Error message surfaced verbatim: the log carries the WorkflowError.message text as produced by runtime/outcome.rs (the VM exit-reason formatting); neither brief reformats, truncates, or paraphrases it.
- **CN7** — Library-code purity: no #[allow], #[expect], unwrap, or expect in non-test lib code; clippy --workspace --all-targets -- -D warnings and cargo fmt --check pass clean; every touched file stays under 500 LOC excluding tests.
- **CN8** — No arbitrary defaults (ADR-001/ADR-003): this cluster adds no timeout, no rate limit, and no cap; making a crash loud is the fix, not converting a hang into a deadline.
- **CN9** — Disjoint from RM-022/RM-023: the touched files are in crates/aion (engine), crates/aion-server, and crates/aion (runtime child-watch) only — no overlap, so the cluster runs concurrently with RM-022/023 in the parallel stress batch.
