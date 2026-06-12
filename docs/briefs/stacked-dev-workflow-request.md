# Request: author the stacked dev workflow in Aion

**Audience:** the Aion authoring agent.
**Framed by:** Waffles (Meridian overseer), on Tom's behalf.
**Status:** request-for-authoring. This describes the *shape and seams* of the workflow we want, in terms of the real `aion_flow` SDK (per `docs/guides/workflows.md`, `docs/guides/activities-and-workers.md`, and the `examples/`). You won't get every internal Meridian flag exactly right — that's expected. Produce a typed Gleam workflow set that **compiles, type-checks, and runs end-to-end today under the `aion/testing` harness**, where each activity's local implementation shells out to the real CLI named here (norn for dev, cargo for checks/gate, the `meridian` binary for provision/land). Leave `// TODO(meridian):` seams for anything that needs a real Meridian command/schema we'll wire later.

---

## 1. Goal

One durable, composable workflow that takes a brief from "nothing" to "landed on main", the way our full stack ideally runs:

1. **Provision** an isolated workspace (worktree / CoW copy / overlay FS / exchange VM).
2. **Concurrently** with dev start-up, **warm the build cache** so the gate later doesn't pay full compile cost.
3. Run the **dev workflow** (an `onatopp-dev-norn`-shaped child): a dev agent plus a tight **scoped verify-fix loop** that runs checks/tests *limited to the edited files* and feeds failures back to the agent.
4. Run the **proper gate** as its own child workflow (full, authoritative checks/tests).
5. Enter the **review loop**: request review, **wait on a signal** for the verdict, and on change-requests go back for another dev round → re-gate → re-review. (Review needs our SDK; today it is a real signal wait you can drive manually.)
6. **Land** (submit + land) once review approves.

Aion is the right home for this because of exactly the things Rhai/YAML couldn't give us: durability across crashes, typed composition, real concurrency, signal waits for human/SDK-scale pauses, and bounded retry loops.

---

## 2. Composition at a glance

```
stacked_dev                         (top-level workflow.define)
│
├── provision_workspace             (activity)          ─┐ await provision,
├── warm_build  +  dev              (workflow.all)       ─┘ then overlap warm_build with dev
│
├── onatopp_dev  (child workflow — spawn_and_wait)
│     ├── dev                       (activity: norn run)
│     └── scoped_verify_loop  (bounded N):
│           ├── scoped_checks       (activity: affected-file clippy/test)
│           └── dev_resume          (activity: norn resume w/ diagnostics)
│
├── gate  (child workflow — spawn_and_wait)
│     └── full_checks               (activity: fmt + clippy --workspace + test)
│
├── review_loop  (bounded M):
│     ├── request_review            (activity: emit request)
│     ├── await_verdict             (workflow.receive on a review signal)
│     ├── dev_resume                (on RequestChanges: norn resume w/ review notes)
│     └── gate                      (re-gate each fix round)
│
└── land                            (activity: stack submit + stack land)
```

`onatopp_dev` and `gate` are **child workflows** (`workflow.spawn_and_wait`), each with its own `[[workflow]]` entry in `workflow.toml` — so they compose, version, and test in isolation. Everything else is an activity dispatched via `workflow.run(...)`.

The Rhai ancestors in the Meridian repo are worth reading for step shapes (the agent has them): `onatopp-dev-norn/workflow.rhai` (dev + verify-fix loop), `run-checks-triaged/workflow.rhai` (check/clippy/test triage), `review-and-land.yaml` (submit + land).

---

## 3. How this maps onto the SDK

- **Top-level + children:** `workflow.define("stacked-dev", in_codec, out_codec, err_codec, execute)` with the codec-shell `run/1` from the canonical pattern. `onatopp_dev` and `gate` are their own `workflow.define`d modules, spawned with `workflow.spawn_and_wait(...)`.
- **Activities:** each unit of real work is `activity.new("name", input, in_codec, out_codec, local_impl)` dispatched with `workflow.run(...)`. **The `local_impl` is the test seam** — under `aion/testing` it runs in-process, so point it at the real CLI (see §4). Deployed, a Meridian worker serves the same activity name. Declare every activity name in `workflow.toml`'s `activities` list.
- **Concurrency:** `provision_workspace` is awaited first (everything needs the `Workspace`); then run `warm_build` and `dev` together with `workflow.all([...])` so the build warms while the agent works. `warm_build` is best-effort — a failure forfeits the warm cache, it must not fail the run.
- **Bounded loops, not engine auto-retry:** engine-side automatic re-dispatch from a `RetryPolicy` isn't wired yet, so model both the verify-fix loop and the review-fix loop as **workflow-driven bounded loops** (recurse with an attempt counter; `workflow.sleep(duration...)` a small durable backoff between rounds; surface a typed `Failed` with the last diagnostics when the cap is hit). Mirror the bounded-attempt pattern in `examples/order-fulfillment/`.
- **The review wait is a signal:** `signal.new("review_verdict", verdict_codec())` + `workflow.receive(...)`, optionally wrapped in `workflow.with_timeout(...)` against a durable deadline so a stale review can time out (the `examples/approval-gate/` pattern, near-exactly). Drive it in tests with `aion signal <run-id> review_verdict --payload '{...}'`.
- **Determinism:** no wall clock / randomness / direct I/O in workflow code — every side effect is an activity. `workflow.now()` / `workflow.random()` if you need either.
- **Optional `query.handler`** for live status ("which phase are we in, which round") — nice-to-have, not required.

---

## 4. Activities — typed I/O and the CLI each local impl shells to *today*

For a first runnable version, each activity's `local_impl` runs the command below in the workspace. Server `http://localhost:29876`, workspace id `2d5fdd51-1f25-45a4-8f86-4d4c978d1355`. Types are what matter for composition; commands are the testable bodies.

### provision_workspace  (activity)
- **In:** `ProvisionInput(brief_id, base_ref, placement, isolation)` — `placement: Local | Remote`, `isolation: Worktree | Copy | Overlay | Vm`.
- **Out:** `Workspace(path, branch, placement, isolation)`.
- **Local impl today (worktree case):** the `meridian` CLI provisions a worktree off `base_ref` and returns its path/branch (the placement/isolation axis exists: `--isolation worktree --placement local`). Model `Copy`/`Overlay`/`Vm` as typed variants with `// TODO(meridian): exchange-VM dispatch` bodies for now.
- **Seam point:** the rest of the workflow must not care *which* isolation it got — only that it holds a `Workspace`.

### warm_build  (activity, concurrent with dev)
- **In:** `Workspace`. **Out:** `BuildWarm(ok, duration_ms)` — advisory.
- **Local impl:** `cargo build` (or `cargo fetch && cargo build`) in `workspace.path`. Best-effort; never fails the run.

### dev  (activity)
- **In:** `DevInput(workspace, brief, design, checklist, stories)`.
- **Out:** `DevResult(session_id, files_touched: List(String), summary)` — `session_id` is essential so later rounds **resume** the same agent.
- **Local impl:** the **norn CLI** runs the dev agent against the brief in `workspace.path` (exactly as `onatopp-dev-norn`'s dev step). This is the step we're surest about.

### scoped_checks  (activity — the "verify" half)
- **In:** `ScopedInput(workspace, files_touched)`.
- **Out:** `CheckResult(verdict, affected_modules: List(String))` where `verdict: Pass | Fail(diagnostics)`.
- **Local impl:** compute affected crates from `files_touched` via the **workspace dependency graph** (libyggd affected-modules — now reliable after a cycle-detection fix landed today), then `cargo clippy -p <affected> --all-targets -- -D warnings`, `cargo test -p <affected>`, `cargo fmt --check`. `run-checks-triaged` is the reference for the verdict/parser shape.
- **No silent fallback:** if scoping yields nothing/unknown, fall back **loudly** to a named wider scope — never silently run zero checks.

### dev_resume  (activity — the "fix" half of both loops)
- **In:** `ResumeInput(session_id, feedback)` (scoped-check diagnostics, or review notes).
- **Out:** `DevResult` (new `files_touched`, same `session_id`).
- **Local impl:** norn CLI **resume** of `session_id` with the feedback injected (the verify-fix resume `onatopp-dev-norn` already does).

### full_checks  (activity, inside the `gate` child)
- **In:** `Workspace`, `files_touched`. **Out:** `GateResult(verdict)` — `Pass | Fail(report)`.
- **Local impl:** `cargo fmt --check` → `cargo clippy --workspace --all-targets -- -D warnings` → `cargo test`. Stricter than `scoped_checks`: scoped_checks is the fast inner loop, `gate` is the trustworthy outer gate.

### request_review  (activity — the SDK seam)
- **In:** `ReviewRequest(workspace, brief, dev_result, gate_result)`. **Out:** `Acked`.
- **Local impl:** emit a review request (a `meridian`/`collective` message, or write an artifact). It only *requests*; the verdict arrives by signal (next item).

### await_verdict  (signal receive, not an activity)
- `signal.new("review_verdict", verdict_codec())`; `workflow.receive(...)` (optionally `with_timeout` against a durable deadline).
- **Out:** `ReviewVerdict(decision)` — `Approve | RequestChanges(notes) | Reject(reason)`.
- **Drive in tests:** `aion signal <run-id> review_verdict --payload '{"decision":"approve"}'`.

### land  (activity)
- **In:** approved `Workspace`, `DevResult`. **Out:** `Landed(pr_url, merge_commit)`.
- **Local impl:** `meridian stack submit` then `meridian stack land` (never manual cherry-pick/merge).

---

## 5. Control flow we care about

- **Parallel start:** await `provision_workspace`, then `workflow.all([warm_build, dev])` so the build warms while the agent works.
- **Verify-fix loop (bounded N):** `dev → scoped_checks → (Fail ⇒ dev_resume → scoped_checks) …`. On cap, typed `Failed` carrying the last diagnostics — never landed, never infinite.
- **Outer gate:** the `gate` child runs once after the verify-fix loop converges.
- **Review loop (bounded M):** `request_review → await_verdict → (RequestChanges ⇒ dev_resume → gate → request_review) …`. `Reject` ends the run `Failed`; `Approve` proceeds. `with_timeout` so a silent reviewer eventually times out, not hangs forever.
- **Land** only on `Approve` **and** a passing `gate`.
- **No silent fallbacks anywhere:** every failure retries (durably, bounded) or surfaces loudly with diagnostics attached.

---

## 6. Definition of done for this request

A Gleam/Aion workflow set (`stacked_dev` top-level + `onatopp_dev` and `gate` children, each with its own `[[workflow]]` + `activities` in `workflow.toml`) that:
- compiles and type-checks the step input→output chain end to end;
- **runs under `aion/testing` with `gleam test`**, every activity's local impl shelling to the §4 CLIs, so the whole pipeline executes against a real brief before the SDK/exchange-VM integrations exist;
- models `await_verdict` as a **real signal receive** (manually drivable via `aion signal`);
- has **bounded** verify-fix and review-fix loops with typed exhaustion and durable backoff;
- keeps **warm_build concurrent** with dev;
- marks every Meridian-specific unknown as a `// TODO(meridian):` seam rather than guessing.

---

## 7. Open questions for Tom / the Aion agent

1. **Affected-module scoping seam:** does `scoped_checks` call the graph via a `meridian` CLI subcommand, or should the Meridian *worker* (not the Gleam workflow) own that and just return the affected set? (The Gleam side stays pure either way; this is about where the graph query lives.)
2. **Outer-gate scope:** always workspace-wide, or "complete affected closure" from the graph? (We run workspace-wide today for trust; the graph could justify a complete-but-narrower set.)
3. **Review signal payload:** just a decision, or structured per-finding notes `dev_resume` consumes directly?
4. **Warm cache sharing:** can the warmed target dir be shared with `gate`/`scoped_checks` (same workspace path), or do CoW/VM isolation boundaries break cache sharing — and does that change whether `warm_build` is worth it per isolation mode?
5. **Loop caps and backoff:** sensible N (verify-fix rounds), M (review rounds), and inter-round `workflow.sleep` backoff — Tom's call, no arbitrary defaults baked in.
6. **One workflow or a family:** is `stacked_dev` one workflow with children, or do we also want the children (`onatopp_dev`, `gate`) independently dispatchable as top-level entry points for partial runs?
