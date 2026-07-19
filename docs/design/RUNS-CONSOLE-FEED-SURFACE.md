# Aion Feed Surface for an External Runs Console

**Status: aion-side surface spec, pre-positioned for the operator's frame
ruling — no frame commitment is made here.** Authored 2026-07-19 from a
full-repo feed inventory (anchors are file:line at aion main `ca7825e6`).
Context: the proposed next frame application is an **Aion runs console** —
live workflow runs as streams in the frame console. Whatever is ruled, this
document records what aion exposes today and the exact deltas an external
streaming consumer needs.

## What exists today

One public WebSocket endpoint, `GET /events/stream`, with five mutually
exclusive first-frame subscription arms (`api/http/router.rs:212-226`,
`api/ws_subscription.rs:88-126`):

| Arm | Content | Durability |
|---|---|---|
| `per_workflow` | one workflow's full history + live tail | **Durable, replayable** via `resume_from_seq` |
| `filtered` | namespace/type-filtered events | Live-only |
| `firehose` | everything | Live-only |
| `cluster` | worker snapshot + connected/disconnected deltas | Live-only, deploy-gated, process-memory |
| `transcript` | one `(workflow, activity, attempt)` agent transcript | Durable spine (`store_seq` cursor); token deltas + overflow events live-only |

Workflow history events (lifecycle, activities, timers, signals, child
correlation) are durable `aion_core::Event` values committed to the `E`
history then broadcast. Transcripts live in a separate durable `O` keyspace.
Worker state is process-memory only. `aion-client` publicly covers workflow
event subscription + describe/history, but **not** cluster, transcript,
attempts, or intervention — those are raw HTTP/WS contracts with
console-owned hand-written DTOs (`aion-client/src/stream.rs:23-38`).

## The three consumer streams vs the ground

**1. Run lifecycle stream — the gap is the cursor.** A runs console wants
"every start/terminal in namespace X, resumable from a cursor". Today
durable replay is only addressable per-workflow once the ID is already
known; the discovery feeds (filtered/firehose) are live-only
(`aion-proto/src/events.rs:201-225`). A disconnected consumer cannot replay
missed lifecycle transitions from one cursor — it must re-list and diff.

**2. Per-step progress — the gap is identity and explicitness.** Durable
history covers schedule/start/retry-failure/completion, but: terminal and
activity events omit `run_id` (only Started/ContinuedAsNew/Reopened/
Paused/Resumed carry one), so multi-run histories require segment
inference; retry is inferred from `ActivityFailed(Retryable, n)` followed
by `ActivityStarted(n+1)` rather than an explicit dispatch event
(`nif_activity_dispatch.rs:545-603`); there is no per-step heartbeat
progress in history. NOI transcripts are per-attempt sockets — a
run-wide progress view must enumerate attempts and open one socket each.

**3. Worker state — the gap is durability, detail, and authority.** The
cluster arm is deploy-gated (full deploy grant, all namespaces exposed:
`stream/cluster_stream.rs:9-17`), non-durable (no event history survives
restart), and thin (no heartbeat timestamps, capacity, in-flight counts, or
queue depth; `NamespaceQuotaState` is namespace-level Claimed counts only,
explicitly excluding Pending backlog).

## Proposed aion-side surface (recommendation)

**F-1 — Durable lifecycle log with a namespace cursor.** A per-namespace
durable log of run lifecycle transitions (started, terminal, paused/resumed,
reopened) with one monotone cursor, exposed as a sixth subscription arm:
`lifecycle { namespace, resume_from }`. This is the piece that makes an
external console possible at all; everything else is enrichment. It reuses
the existing commit path (events are already durable per-workflow — this
adds a namespace-ordered index, not a second write).

**F-2 — `run_id` on every event.** Schema evolution so terminal and
activity events carry the run identity their consumers currently infer.
Additive field; old readers unaffected.

**F-3 — Public client + published contract.** `aion-client` grows cluster/
transcript/attempts/lifecycle targets, and the ts-rs export set grows the
`Streamed*` wrapper and subscription-request types that are currently
console-only hand-copies (`aion-core/src/generated_types.rs:73-103`). One
generated contract, two consumers (ops console + frame app), zero drift.

**F-4 — A read-only worker-state capability.** Namespace-scoped, read-only
worker feed grant, distinct from the deploy grant (ADR-022 alignment). The
worker *detail* enrichment (heartbeats, in-flight, capacity) lands with the
worker-lifecycle build's W-5 read model — this spec deliberately does not
duplicate it; the runs console consumes W-5's stream through F-4's grant.

**F-5 — Fix the two observed console/server contract mismatches** (real
defects found by the survey, worth fixing regardless of the frame ruling):

- The console sends `after_seq` as its resume cursor on workflow/filtered/
  firehose subscriptions; the server only recognizes per-workflow
  `resume_from_seq`, and filtered/firehose have no cursor at all
  (`websocket-protocol.ts:41-87` vs `api/ws_subscription.rs:139-155`). The
  sent field never activates server replay — resync works only because the
  console refetches history around it.
- The console's WS manager multiplexes several logical subscriptions onto
  one socket; the server honors one subscription per socket and ignores
  post-subscribe frames (`websocket.ts:58-76,285-305` vs
  `stream/socket.rs:143-150`). Only the first filter is real; the rest is
  local client-side matching over an over-broad feed.

**Incidental finding:** `WorkflowTimedOut` is an exported, readable event
type with no production emission site — either an unimplemented transition
or a dead type; it should not appear in a public contract until it is real.

## Sequencing and ruling points

F-5 and the incidental are hygiene — dispatchable now, small. F-1 is the
load-bearing unit and the only one the frame app hard-requires; F-2/F-3
ride behind it; F-4 lands with (not before) the worker-lifecycle W-5 read
model. For the operator: (1) whether the runs console is the next frame
app (Waffles' recommendation — this spec is its aion half either way);
(2) F-1's arm shape (sixth arm on the existing endpoint, recommended, vs a
separate endpoint); (3) whether F-5 dispatches immediately as hygiene.
