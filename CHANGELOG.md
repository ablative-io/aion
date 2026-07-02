# Changelog

All aion crates share one workspace version; entries below cover the
whole stack (crates.io) plus the `aion_flow` Gleam SDK (hex) where noted.

## Unreleased

### Authoring (types-first, ADR-014)

- **Types-first codec generation; schema-first removed.** The authored
  source of truth is now the Gleam types module `src/<package>_io.gleam`
  (types only). `aion generate` reads it via
  `gleam export package-interface` and derives the codecs module
  (`src/<package>_codecs.gleam`, now carrying the encoders/decoders), the
  EMITTED `schemas/*.json` artifacts (marked, never authored), and the
  activity plumbing. **`aion codegen` is removed** — migrate by stripping
  the generated header and functions from the io module, deleting the
  authored schemas, and running `aion generate .` (full recipe in
  docs/guides/codegen.md). The test scaffold is now write-once (an
  author-filled scaffold is never overwritten, and `--check` only requires
  it to exist). `examples/order-saga` is migrated and drift-gated in CI;
  its wire shapes are unchanged (pinned by the regenerated wire-compat
  golden and a semantic schema-equivalence gate).

### Engine — in-VM execution tier (CUT 3)

- **`InVm` activities now execute for real**: a new engine NIF
  `aion_flow_ffi:dispatch_activity_in_vm/4` spawns the SDK-composed runner
  thunk as a LINKED child process of the workflow process (beamr 0.12.0's
  `Scheduler::spawn_link_closure`, which deep-copies the thunk's environment
  into the child heap). Recorded-result semantics are identical to remote
  activities by construction: the same ordinal/correlation allocation, the
  same `ActivityScheduled`/`ActivityStarted`/terminal shape (task queue,
  node, and attempt stamped; NO event-schema change — the tier is routing,
  not recorded), the same correlation-keyed completion delivery the await
  path records. The runner runs once live; replay returns the recording
  without re-execution; a runner crash surfaces as a terminal
  `ActivityFailed` (the workflow process survives); node death after
  `Started` recovers through the existing replay-reopen path (the SDK
  re-supplies the thunk on every replay); `with_timeout` scope expiry over a
  hanging runner records the durable timeout failure unchanged. Defenses:
  tier `in_vm` on the arity-3 remote wire and in-VM members in `collect_*`
  fan-outs are refused before anything is recorded. `aion_flow` gains
  `activity.execution_tier`/`selected_tier` (see its own changelog). New
  worker-free example: `examples/invm-demo`.

## 0.6.1 — 2026-06-13

### Engine (via beamr 0.6.1)

- **Workflow-process heap-reservation fix.** Pinned beamr 0.6.1, whose
  interpreter now runs `ensure_space` before the `put_list`/`put_tuple2`
  allocations, so a data-dependent burst of cons/tuple construction — e.g.
  decoding a large stage report inside workflow code — triggers GC and
  heap growth instead of dying with a fatal `heap full` that bypassed the
  collector. Surfaced by the brief_dev real-norn dogfood: a 12 KB scout
  report decoded in the workflow process crashed the run silently right
  after scout while a 10 KB one survived — a heap-reservation cliff, not
  genuine exhaustion (the process heap grows to ~1 MB). No aion code
  change; the bump adopts the VM fix. `aion_flow` is unchanged at 0.4.0.

### Notes

- Workflow code should stay thin (ADR-012): large activity results are
  best threaded as opaque payloads and decoded only by the consuming
  activity on the worker, where the heap is full-size and the work runs
  once rather than on every replay. The thin-workflow reshape is tracked
  as RM-023 and the standard library that makes it cheap as RM-022;
  observability so a crash never again looks like a hang as RM-024.

## 0.6.0 — 2026-06-13

### Engine

- **No default activity timeouts.** The engine-imposed 30s activity
  dispatch timeout is gone. Activity waits are unbounded and terminate
  only on completion, worker loss, server shutdown, or a workflow-level
  timeout the author chose. Agentic activities that run for an hour-plus
  are first-class.
- **Worker loss now delivers.** Losing the worker mid-activity fails the
  dispatch with a typed error instead of leaving the workflow parked
  forever (stream teardown previously only deregistered the worker).

### CLI

- **`aion new`** — scaffold a workflow project from four embedded
  templates: `hello_world`, `saga`, `approval_flow`, and `dev_pipeline`
  (the stacked-dev agentic pipeline, `--worker rust`). Scaffolds build,
  package, and pass their own test suites out of the box.
- **`aion codegen <dir>`** (and `--check`) — generate Gleam types and
  JSON codecs from JSON Schemas, with loud typed errors carrying file
  and RFC 6901 pointer context.
- Stale command hints in CLI output corrected for the unified `aion`
  binary.

### SDKs

- `aion_flow` **0.4.0** (hex) — `testing.mock_child`: typed child
  workflow doubles for unit-testing parent workflows without running
  their children. Scaffold templates now pin
  `aion_flow >= 0.4.0 and < 0.5.0`.
- `aion-rs` testing harness gains the matching in-process child doubles.
- Worker SDK logs session establishment; reconnect behavior hardened
  against server restarts.

### Examples

- `examples/stacked-dev` — the full agentic dev pipeline (provision →
  agent dev rounds → scoped verify → workspace gate → human review
  signal → land) with a standalone Rust activity worker, proven live
  against the real yg/norn/cargo/meridian CLIs end to end.
- Nested-workflow e2e suite: three-level chains, recursion,
  recovery-at-depth, and cancellation semantics pinned.

## 0.5.0 — 2026-06-11

- Unified `aion` binary (server runs as `aion server`; `aion-server` is
  lib-only).
- First release validated outside-in end to end (deploy → start →
  signal → query → recover).
