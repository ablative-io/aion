# Aion — CLI/UX Feedback & Distributed Direction

> Written 2026-06-22 by Claude, from **heavy real use this session**: dozens of `aion start` /
> `describe` / `list` / `signal` / `cancel` calls driving the brief-dispatch loop (Norn dev agents via
> `stacked_dev`). Part 1 is concrete friction I actually hit, prioritized. Part 2 is the distributed
> direction Tom sketched + my take on feasibility and sequencing. Honest, not flattering.

---

# Part 1 — CLI/UX friction (near-term, actionable)

The engine is solid; the **operator surface** is where it bites. Almost every command needed
`| python3 -c '...'` to be usable. Ranked by how much pain it caused.

## P0 — the daily-driver papercuts

1. **`list` / `describe` emit raw JSON with no human view.** Every status check was
   `aion list --namespace stacked | python3 -c '...parse...'`. There's no table, no column output, no
   sorting by start time. → A default human table (`STATUS  TYPE  ID(short)  STARTED  AGE`) with
   `--json` to opt back into machine output. This alone removes ~80% of the friction.

2. **No way to tell "developing" from "parked" from "hung" — they're all `Running`.** A `stacked_dev`
   parked on the `review_verdict` signal, one actively working, and one silently stuck all show the
   same `Running`. I had to infer "parked" from a `TimerStarted` + `ChildWorkflowCompleted` history
   tail, and "hung" from comparing **worktree file mtimes across cycles** (the engine gave me nothing).
   → Surface a richer run-state: `Running(awaiting signal: review_verdict)`, `Running(activity: brief_dev, last progress 3m ago)`. Even just "blocked-on-signal `<name>`" in `list` would be transformative.

3. **`cancel` doesn't cascade to children, and needs the FULL id.** Cancelling a `stacked_dev` left its
   `brief_dev` child running as a zombie — I had to find the child's full id in `list` and cancel it
   separately. And an 8-char prefix errors out. → `cancel` should (a) accept an unambiguous prefix,
   (b) `--cascade` (or default to cascading) to children, (c) print what it cancelled.

4. **Signal payloads are undiscoverable + unvalidated.** I only knew `review_verdict` +
   `{"decision":"approve"}` (and the accepted value set) from reading the workflow source. A wrong
   payload is silently accepted (`{"accepted":true}`) with no schema check. → `signal --help` per
   workflow (or `describe` listing the signals a run is waiting on + their expected shape), and
   validate the payload against that shape at accept time.

## P1 — visibility into *why*

5. **Failure/verdict detail is truncated or absent.** `dev_failed` gave `provider error: stream error
   … help.openai.com` with no way to see the full activity error; a `gate` child showed `Completed` but
   its **verdict (pass/fail) wasn't in the summary** — I inferred gate=pass from "the parent is still
   parked." → `describe <id> --activity <name>` (or `--errors`) showing full activity I/O + the
   structured result/verdict of child workflows, not just their lifecycle status.

6. **No `logs` / `tail`.** To see what a dev agent was actually doing I had to read Norn session files
   and worktree mtimes. → `aion tail <id>` streaming activity stdout/events would remove the
   "is it alive?" guesswork entirely (and is a prerequisite for the monitoring in Part 2).

7. **No liveness/heartbeat.** "Hung" detection was 100% manual (mtime diffing across 30-min cycles).
   The engine knows when an activity last made progress; expose it. → an activity-level
   `last_heartbeat` / `last_progress_at`, and optionally a stall watchdog the server can flag.

8. **`list` is namespace-scoped with no "show me everything."** I ran the whole loop in `stacked` and
   never saw a `meridian_dev_pipeline` running in `default` until I explicitly probed every namespace.
   → `--all-namespaces` (and/or show namespace as a column). An operator should never miss a running
   workflow because they didn't guess the namespace.

## P2 — defaults & ergonomics

9. **The 5-minute `review_deadline_ms` default forced a `jq` rewrite on every dispatch.** Every
   `start` was `aion start stacked_dev --input "$(jq -c '.review_deadline_ms=7200000' <file>)"` so runs
   would *park* at review instead of failing when no reviewer signalled in 5 min. → a saner default for
   human-in-the-loop review, or a `--set key=value` flag so I don't need `jq` to override one field.

10. **`start --input "$(cat …)"` is the only input path.** A `--input-file <path>` would avoid the
    `$(cat)` dance and the shell-quoting hazards.

11. **No `start --watch` / no run URL.** After `start` I got an id and then had to poll `describe`. A
    `--watch` that blocks and prints state transitions (or at least a one-liner "watch with: aion tail
    <id>") would close the loop.

12. **`land` assumes a clean target tree.** The pipeline's `land` (`yg branch merge`) failed whenever
    `main` was dirty (a tracked `.meridian/.resource-manifest.json` churns constantly), so *every*
    approve fell back to a manual `git merge --no-ff` harvest. Not strictly an Aion bug, but the
    failure mode surfaced through Aion and the error was opaque. → clearer land-failure reporting, and
    consider landing being tolerant of unrelated dirty paths.

**If only three things got done:** (1) human table output for `list`/`describe`, (2) a distinct
"blocked-on-signal / last-progress" run-state, (3) `cancel --cascade` with prefix ids. Those three
would have removed most of my python-parsing and all of the zombie-child / hung-guessing pain.

---

# Part 2 — Distributed Aion (Tom's direction + my take)

**Tom's sketch:** today Aion is a single server. He wants it to **deploy workers as well as
workflows** — send one command to the server and it provisions workers (with their tasks) on *other*
machines ("remote deploy rather than pool-and-start-up by hand"); optionally **package a workflow
together with the worker that runs it** as one deployable unit; and eventually **monitor + autoscale**
(spin up more workers when needed). The "task-use system" that decides what runs where is deferred —
Tom will spec it later.

## My take: very doable, and Aion is unusually well-positioned for it

The genuinely hard part of distributed execution — **durable, exactly-once, replayable work** — Aion
**already has** (event-sourced history + deterministic replay, verified). Most systems bolt durability
on *after* going distributed and suffer for it; Aion has it first. What's missing is a **control
plane**, not a new execution model. That's a much friendlier place to start from.

### What has to exist first (and partly doesn't yet)

From the June deep-dive, three relevant gaps stand between here and distributed:
- **Single-node ownership isn't enforced** — `fs4` is declared but unused; there's no lock/lease. A
  distributed system needs work **leasing/ownership** so two nodes don't run the same activity. This
  gap becomes the *first real feature*, not just a cleanup.
- **Server-initiated per-activity cancellation isn't wired** (only worker-shutdown). Remote workers you
  can't individually cancel/drain are hard to operate. Needed for deploy/scale-down.
- **No engine-side retries / no heartbeat surfacing.** Both are prerequisites for trusting remote
  workers (you must detect a dead remote worker and re-lease its work).

These line up *exactly* with the Part 1 P1 items (heartbeat, liveness, cancellation). **That's the
key insight: the near-term UX work and the distributed work are the same foundation.** Heartbeats you
add for `aion tail`/hung-detection are the same heartbeats a scheduler uses to detect a dead remote
worker and re-lease its tasks.

### A sequencing I'd recommend (each step independently useful)

1. **Worker registry + heartbeat.** Workers register with the server (id, namespace, capabilities,
   capacity) and heartbeat. The server gets a live view of the fleet. *This is the linchpin* — it
   powers monitoring (Part 1 #7), `aion workers` listing, AND the distributed scheduler. Start here.
2. **Work leasing / ownership** (close the `fs4` gap). Activities are leased to a worker for a TTL,
   renewed by heartbeat, re-queued if the lease lapses. This is what makes multiple workers — local or
   remote — safe. Durability already makes re-execution correct.
3. **Remote worker deploy (control plane).** `aion worker deploy --node <host> --namespace stacked
   --count N` → server instructs a small **agent** on the remote host to start workers. Needs a tiny
   per-host daemon (the thing being deployed *to*) + a signed deploy command (Meridian already has
   signed remote dispatch into VMs via `meridian-vm-daemon` — reuse that trust/transport rather than
   reinventing it).
4. **Packaged workflow + worker bundle.** A deployable unit = workflow definition(s) + the worker
   binary/image that executes their activities + required capabilities. `aion deploy bundle.toml`
   ships both. This is the clean version of "deploy the workflow and the worker together" — the worker
   is declared *by* the workflow bundle, so the server knows what to provision where.
5. **Autoscale + monitoring.** Once 1–2 give you queue depth + worker liveness, a policy ("if pending
   activities in namespace X > threshold and capacity < cap, deploy K more workers") is a small loop on
   top. Do this last; it's cheap once the registry/leasing exist, and dangerous before they do.

### Caveats / opinions

- **Don't rebuild Temporal or k8s.** Aion's edge is beamr/Gleam-native durable execution for the
  *agent-dispatch* use case. Keep the control plane minimal and owned; resist generic-orchestrator
  scope creep. The remote-deploy daemon should be tiny.
- **Reuse Meridian's signed remote dispatch** (`meridian-vm-daemon`, `meridian-trust` Ed25519/CA) for
  the deploy transport — you already built the hard security part; don't grow a second one.
- **The registry is a shared seam with the Norn session daemon** ("tmux for agents") — both want a
  live registry of running agent/worker processes. Design the registry once, used by both, or they'll
  diverge. (See the norn session-daemon notes.)
- **Order matters more than features here.** Leasing before remote-deploy, registry before autoscale.
  Shipping remote-deploy without leasing gives you double-execution races that durability *hides*
  (replay makes them correct but wasteful) — you'd burn tokens silently. Leasing first.

**Bottom line:** the distributed vision is right and reachable, and the cheapest path to it runs
*through* the CLI/observability work you already need — heartbeat + registry + leasing are the common
substrate for "better feedback" and "deploy workers remotely" alike. Start with the registry; it pays
off immediately as monitoring and is the foundation everything else hangs off.

*(Deferred per Tom: the "task-use system" that decides placement/what-runs-where — to be specced
later. It slots in above leasing, as the policy layer that chooses which worker/bundle handles which
task.)*
