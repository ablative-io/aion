# Workflow resilience & self-healing — design notes

> Status: design notes, not yet briefs. Captured 2026-06-16 after two local
> stacked-dev runs failed at `dev_review` on a transient provider error.
> Decision so far: build **L3 (resume) first**, then **L1+L2 (auto-retry)** as
> one follow-on. Nothing here is implemented yet.

## 1. The incident

Two local `stacked_dev` workflows reached the `dev_review` step and failed with:

```
norn review failed — exit status 1: norn: agent error: provider error: rate limited
```

The operator's provider limits were **not** actually exhausted, so this was
almost certainly a transient provider-side throttle (429) or a brief upstream
blip. Both workflows had already completed `scout` and `dev` (roughly an hour of
agent work each), and that work was lost when the workflow failed terminally.

LLM/agent steps are the failure-prone ones — rate limits, provider outages,
overload, network blips — and most of those are not reasons to abandon an
hour-long run. They are reasons to wait and retry, or to resume.

## 2. Verified current state (root cause)

Two compounding causes, both confirmed in code:

1. **Handlers classify every failure as terminal.** The stacked-dev worker
   handlers (`examples/stacked-dev/worker/src/handlers.rs`) build only
   `ActivityFailure::terminal(...)` for every non-zero CLI exit — including
   "rate limited". They never emit retryable, even though the worker SDK has
   `ActivityFailure::retryable()` (`crates/aion-worker/src/activity.rs:42`).

2. **No retry policy is attached, and the engine wouldn't honour one anyway.**
   - `aion_flow` has a `RetryPolicy(max_attempts, backoff)` type with
     `Exponential`/`Linear`/`Fixed` backoff and an `activity.retry()` decorator
     (`gleam/aion_flow/src/aion/activity.gleam`). It is **opt-in** and
     explicitly documented as "no default… runs exactly once." None of the
     stacked-dev activities attach one.
   - **The engine never consumes `RetryPolicy`.** It is stored on the Gleam
     activity value but is never threaded into the dispatch config and never
     read by the engine (no reference in `nif_activity_dispatch.rs`,
     `nif_activity.rs`, or `crates/aion/src/activity/`). The policy is inert.
   - **There is no retry driver.** Nothing re-dispatches on a retryable
     failure. Proof: the durability resolver test
     `rejects_non_terminal_activity_failure_as_history_shape_error` treats a
     lone `Retryable` `ActivityFailed` with no later outcome as a
     **HistoryShape error**. The cursor/resolver can *replay* a retry chain
     that already exists in history (`walks_retry_failures_to_eventual_activity_success`)
     and guards that terminal resolutions are terminal — but nothing *produces*
     the next attempt.

What already exists (the scaffolding): the `Retryable`/`Terminal` taxonomy
(`crates/aion-core/src/error.rs`), the wire encoding of the kind, the SDK
`retryable()` constructor and `RetryPolicy` type, and the durability history
model that can replay multi-attempt chains. What's missing: classification at
the boundary, the engine retry driver + policy plumbing, and a resume path.

## 3. Failure taxonomy — three buckets

Two buckets (retryable/terminal) are not enough. Three:

- **Transient / auto-retryable** — rate limit, provider 5xx/overload/429/503,
  network blip, timeout. Clears on its own → retry with backoff; the workflow
  never observes it.
- **Operator-recoverable** — binary not installed, wrong GitHub access, missing
  credential, disk full. Blind retry won't help, but it *is* recoverable once a
  human/system fixes the cause → fail into a **resumable** state, fix it, resume.
- **Terminal / business outcome** — the review found real issues, the gate
  genuinely failed, a schema violation, a deterministic bug. Real answers, not
  faults → surface, don't retry.

## 4. Mechanism — three layers

### L3 — Resume a failed workflow (build first)

Because aion is event-sourced, a `Failed` workflow still holds its whole
history — `scout`, `dev`, `warm`, `scoped` all completed and recorded. Resume =
re-drive from the failed step; replay returns the recorded results for
everything already done, so **only the failed step re-runs**.

- Covers all three buckets (transient, operator-recoverable, even
  misclassified-as-terminal) with no dependency on classification.
- **Activities are at-least-once across a crash.** If the engine crashes
  after an activity completes but before the await records its result, the
  activity may be re-dispatched on recovery (or resume). Activities must
  therefore be idempotent. (This is independent of the retry work above:
  it holds today, with or without a retry driver.)
- Folds in the already-needed `aion recover` CLI (manual DB surgery was done 3×
  in the prior session).
- Teardown already preserves the worktree/branch/norn session on failure, so
  the state needed to resume survives.
- norn's `--resume-if-exists` / `--resume` session model means a re-run agent
  step **continues** its session rather than starting cold — cheap, no lost work.

**Open design question (the crux):** resume must reconcile with the load-bearing
invariant "status is a projection; each terminal status has exactly one terminal
event" (CLAUDE.md #4) and the Resident/Suspended residency flag. A `Failed`
workflow has a `WorkflowFailed` event; history is append-only, so resume cannot
delete it. The likely shape is a new compensating event (e.g. `WorkflowReopened`
or an activity-level retry-requested event) that supersedes the terminal failure
in the projection, returns the workflow to `Resident`/`Running`, and triggers
re-dispatch of the failed step. This decision touches durability and needs
explicit review before implementation. The existing recovery machinery
(`startup.rs` `register_recovered_resident`, `crates/aion/src/durability/recovery.rs`
which already issues `Command::RunActivity` to re-drive) is the foundation to
build on.

### L1 — Classify at the boundary (part of the auto-retry build)

CLI-running activities (norn, yg, git, collective) map known-transient signals
to retryable instead of blanket-terminal. Cleanest: a machine-readable signal
from norn — a distinct exit code (sysexits `EX_TEMPFAIL` = 75) or a JSON error
category — rather than string-matching stderr. String-match ("rate limited",
"overloaded", 503, "connection reset") is an acceptable stopgap.

### L2 — Per-activity retry policy + engine driver (part of the auto-retry build)

Attach `activity.retry(max, Exponential(...))` to the norn-backed steps; the
engine re-dispatches retryable failures with backoff and the workflow doesn't
observe them. Notes:

- Respects the "no short timeout" rule — the budget is attempts + backoff
  ceiling, **not** a wall-clock cap; a >1hr agent step is fine.
- The hard part (deterministic replay of retry chains) is already built in the
  cursor/resolver; the build is the **driver** (retryable → backoff timer →
  re-dispatch attempt N+1 → cap at `max_attempts` → terminal) plus threading the
  SDK policy through the dispatch config into the engine.

**L1 alone is unsafe without L2's driver:** if a handler starts emitting
`Retryable` today, the engine has no driver to re-dispatch it and the durability
layer chokes on a lone retryable failure (history-shape error) — worse than the
clean terminal we get now. So L1 and L2 land together as one deliverable.

## 5. Default posture for agent steps

Classify transient → retry with generous backoff; on exhaustion, leave the
workflow **Failed-but-Reopenable** (don't tear down), and print a
`reopen with: aion reopen <id>` hint the way the pipeline already prints the
review-signal commands.

## 6. Sequencing & rough effort

- **L3 (resume) — first.** Smallest path to "stop losing hour-long runs",
  works for all failure types, no new engine retry machinery. Investigation +
  design decision (how resume reconciles with status-is-projection) is the bulk;
  implementation builds on existing recovery infra. Likely lives in
  `aion-operations` or `aion-durability`.
- **L1 + L2 (auto-retry) — one follow-on deliverable.** A genuine build:
  engine retry driver + policy plumbing + boundary classification. Bigger than
  L3 but well-scoped; the determinism-hard part is already done.

## 7. Related

- `AW-014` (worker round-robin + closed-channel fallback) — unrelated to this,
  but the same dispatch subsystem; small net-deletion brief, ready to implement.
- Prior restart-resilience work (collect_all recovery, namespace recovery,
  session resume) is the closest existing machinery to L3.
