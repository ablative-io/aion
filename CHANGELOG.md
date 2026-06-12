# Changelog

All aion crates share one workspace version; entries below cover the
whole stack (crates.io) plus the `aion_flow` Gleam SDK (hex) where noted.

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
