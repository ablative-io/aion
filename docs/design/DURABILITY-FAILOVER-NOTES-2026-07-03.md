# Durability & Failover Semantics — Working Notes (2026-07-03)

Capture of Tom's post-kill-9 thinking, verbatim-in-spirit, before the Meridian
handoff. These are direction-setting notes, not a finished design. Everything
here feeds the existing ledger (#197, #206, #207, #208, #147, #186) plus the
new items created alongside this doc (dead-man switch, kill matrix,
disk-bound-workspace failover design).

## 1. The orphaned-harness problem and the dead-man switch

Observed live: kill-9 of server + worker leaves norn running. The agent
finishes its work, produces its structured output and dev notes — and nobody
is listening. The recorded history has no result for that step, so on resume a
FRESH attempt spawns in the same workspace, finds the work "basically done",
and the end-of-run value (dev notes, report, structured output) is lost. Two
agents in one workspace is also a correctness hazard, not just waste.

Two complementary answers:

- **Dead-man switch (near-term, norn-side).** In driven/JSON-RPC mode the
  worker holds norn's stdin pipe open. When the worker dies — even kill-9 —
  the OS closes that pipe and norn reads EOF. A dead-man mode says: on stdin
  EOF, abort the run and exit immediately. No heartbeats needed; the pipe IS
  the pulse. Opt-in (we would not want this in every mode), but right for
  worker-owned agent steps. This prevents the double-agent hazard and stops
  orphans burning tokens on work nobody will record.
- **Orphan adoption (#208, the richer answer).** Worker keeps durable attempt
  state (haematite in the worker SDK); on restart it finds the orphan's
  session and re-adopts via `--resume-if-exists` instead of spawning fresh.
  Preserves the orphan's work rather than discarding it.

These compose: dead-man for correctness now; adoption to preserve value later.
Tom: "not a right-now problem, but something we need to address or think about
soon."

## 2. Disk-bound workspaces × cross-node failover (the big one)

Scenario: provision clones the repo to local disk on node A
(`~/.aion/clones/<run_id>/repo`). The node dies mid-dev-step. The workflow
fails over to node B. Replay does NOT re-execute provision — its recorded
result is just a path string, which on node B is a dangling reference. The
next agent attempt starts with "where's the repository?".

This is exactly where node affinity earns its place: work on local files is
node-bound by nature. But pinning alone just converts "wrong answer" into
"wait forever". The pieces Tom sketched:

### a. Failover policies (per step / per workflow)

A declared answer to "this step didn't finish and its node is gone":
- **restart-workflow** — the work was disk-only, treat as catastrophic for
  this run, start over;
- **redo-from-step** — re-run from a named earlier step (e.g. re-provision,
  then re-run the dev step);
- **wait-for-node** — the node is expected back (reboot case: the files are
  still there; the same node resuming is fine and already works);
- **resume-from-checkpoint / reconstruct** — see below.

### b. External checkpoints

A stacked-diff-style workflow that regularly commits, pushes, and opens PRs is
creating durable external checkpoints as a side effect. Failover can honestly
say "the last state persisted off-box was commit X" and resume from there,
accepting the loss of anything past the checkpoint. Checkpoint-aware resume is
a policy, not magic: work executed past the checkpoint is lost unless (c)
applies.

### c. Event-sourced workspace reconstruction ("the machine gun")

The idea that changes the game, and it is only possible because norn is ours:
if every tool call the agent made is durably recorded (not token deltas —
tool-call events, in order), then a lost workspace is recoverable ANYWHERE:

1. re-clone the repo on the new node;
2. replay the recorded write/edit tool calls in order — machine-gun them out;
   seconds, not minutes;
3. the workspace is now byte-identical to where the agent was;
4. if the norn session record (currently a local jsonl) is ALSO persisted to
   haematite, the agent session itself can resume on any device, any
   directory — mid-step, at the exact tool call where it died.

Caveats that make this a design problem, not a weekend hack:
- **Effect classification.** File writes are locally replayable. Creating a
  PR, sending a message, calling an external API are NOT — replaying them
  duplicates external side effects. Reconstruction must replay local effects
  and fence external ones (skip + trust the original occurrence, or
  idempotency-key them).
- Read-tool calls need no replay; only mutations.
- Shell commands are the hard middle: some are pure-local (`cargo build`),
  some are external (`git push`, `gh pr create`). Probably: replay nothing,
  reconstruct from file-state mutations only, and treat shell-produced
  artifacts as rebuildable (or checkpoint them).

### d. Failure-value categories

Three honest tiers of what a failure costs, worth naming in the design:
1. **Irrecoverable** — the work existed only on destroyed disk, no
   checkpoints, no event record: restart from the beginning (the asteroid).
2. **Checkpointed** — resumable from the last external checkpoint (git
   push/PR); loses the tail.
3. **Fully event-recorded** — reconstructable to the exact tool call;
   loses (almost) nothing.

The engineering goal is moving as much as possible from tier 1 toward tier 3.

## 3. Doctrine: the total-failure standard

Tom, verbatim-in-spirit: it is a **total failure — 100% on us** — if any part
of the infrastructure we ship OR require someone to build loses work on a
crash. That includes user-written workers. A Python developer with sloppy
code, no uv, crashing workers: "if we've told them this is a durable workflow
engine, then it needs to withstand their shitty code." This is the BEAM
philosophy inherited through beamr: your code is going to fail — don't worry
about it. We stack the deck for ourselves with compile-time checking (Gleam,
Rust), but we don't control the world. **In any event short of the entire
world being destroyed, the work must be recoverable on the other side.**

Immediate consequences already on the ledger: #197 (engine ignores retry
policies — worker death under a live server is currently a terminal workflow
failure, which flunks this standard), #207 (graceful shutdown fails in-flight
dispatches — a polite restart is currently more dangerous than kill-9), #208
(worker durable attempt state).

## 4. Peer connectivity, auto-discovery, and the buddy

Status check (Tom asked): **not built.** What exists: multi-node clustering
works when configured explicitly (static peer config — the multi-process
kill-9 failover proofs all ran this way), shard election, adoption, placement.
What does not exist: auto-discovery (#147, mDNS-first cut, still backlog) and
the **buddy behaviour** — Tom's original plan, worth restating because it is
not on the ledger anywhere:

> `aion server` boots → looks for peers (auto-discovery) → finds none → spins
> up a buddy node so there are always two. If the owner dies and the buddy is
> left alone, the buddy adopts the work AND spins up a new buddy of its own.

Self-healing minimum-redundancy. Folds naturally into #147 but is a distinct
behaviour on top of discovery.

## 5. The kill matrix

Kill-the-whole-world recovers (proven live). Partial kills are the honest
gaps. Systematic experiment, CLI-driven (no UI needed), Tom has sanctioned me
driving it myself: capture PIDs, then for each combination record
recovers/fails/orphans:

- kill −9: server only / worker only / norn only / server+worker /
  worker+norn / all three
- graceful: server restart / worker restart (drain)
- single-node and (later) two-node variants of each

Predictions to verify: worker-only death under live server = synthesized
terminal failure (#197); graceful drain fails in-flight (#207); norn orphaned
by worker death (§1). The matrix result becomes the acceptance sheet for
#197/#207/#208.

## 6. Multi-worker ecosystems (confirmed) and the tiny-worker vision

Confirmed: **one workflow can call activities served by many different
workers.** Routing is per-activity — task_queue and node are set on the
activity (NSTQ-4/NODE-4), so a workflow can run its agent steps on the laptop
(where the repo lives — affinity again) and its embedding step on the Linux
box with the GPU, each served by a different worker on its own queue.

Tom's vision to preserve: a **cloud of tiny single-purpose workers** —
each a single uv-script Python file that does exactly one thing (embeddings;
text extraction; TTS; entity extraction), so simple they can't fail, no
package hell, deployed as a fleet. Python is and will remain the easy path
for ML-adjacent work; the platform should make a one-file Python worker a
first-class citizen. (Relates: #190 all-purpose worker; Phase 3 worker
deploy.)

## 7. Worker artifacts in haematite

Question: can we store worker binaries in the database? Yes — and half of it
already exists: `.aion` packages are content-addressed blobs in haematite
with cluster-wide lazy pull (#117). A Python worker is just text — trivial.
Native binaries (Rust/Go) work the same mechanically: store the blob,
materialize to disk, `chmod +x`, exec. The real issues are not "OSes distrust
materialized blobs" (they mostly don't for files a local process writes;
macOS quarantine applies to browser-style downloads) but:
- **platform/arch matching** — a mac-arm64 binary is useless on linux-x86_64;
  worker artifacts need per-target builds and a target-aware pull;
- signing/trust story for anything crossing machine boundaries;
- versioning + rollout.

All of this folds into Phase 3 / WORKER-DEPLOYMENT.md (#186).

## 8. Sequencing (Tom's explicit call)

Remote worker deploy and auto peer discovery are important — **but the
durability fundamentals come first.** "If we got them right while these
things were still wrong, it'd make the current things harder to solve."

Order of operations this implies:
1. Durability floor: #197 engine retry, #207 graceful-shutdown parity,
   #206 durable interventions, dead-man switch, kill matrix as the proof.
2. Disk-bound workspace failover semantics (§2 design pass: policies,
   checkpoints, reconstruction).
3. Then worker-deploy-through-service (#186) and discovery/buddy (#147) on
   top of foundations that hold.
