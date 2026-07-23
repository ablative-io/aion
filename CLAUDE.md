# CLAUDE.md

## What This Is

Aion is a general-purpose **durable workflow engine** — Temporal-class durability built on Gleam, Rust, and the BEAM, running on the **beamr** VM (our Rust BEAM implementation, a crates.io dependency). Durable execution means a workflow can crash on step nine of ten and resume exactly where it was, or sleep for three months and wake in the same logical state. The mechanism is **event-sourcing plus deterministic replay**: workflow code is re-executed from the start on recovery, and every side-effecting call returns its recorded result from history instead of acting again.

Aion is general purpose. Meridian is the first consumer, not a constraint.

The design lives under `docs/design/` — 30 clusters. JSON is the source of truth (`design.json`, `checklist.json`, `stories.json`, `briefs/*.json`) where a cluster carries those sources; markdown is rendered output. The whole-system picture is in `docs/design/workflow-engine/DESIGN-OVERVIEW.md` and `COMPONENT-ARCHITECTURE.md` — read those first.

## Architecture

The Rust crate family is the `crates/` workspace membership in the root `Cargo.toml`:

- **`crates/aion-awl`** — the AWL lexer, parser, checker, canonical printer, schema derivation, and compiler.
- **`crates/aion-awl-lsp`** — the AWL Language Server Protocol adapter.
- **`crates/aion-awl-package`** — assembles compiled AWL workflows into `.aion` archives.
- **`crates/aion-core`** — pure domain model: events, payloads, identifiers, `WorkflowStatus`, filters, and errors.
- **`crates/aion-store`** — persistence contracts, the in-memory reference store, and backend conformance tests.
- **`crates/aion-store-libsql`** — the alternative durable libSQL backend.
- **`crates/aion-store-haematite`** — the default durable backend, with single-node and distributed modes.
- **`crates/aion-package`** — `.aion` archive validation, content hashing, and module namespacing.
- **`crates/aion-toolchain`** — the server-side Gleam compile, type-check, and package adapter.
- **`crates/aion`** — the transport-agnostic engine (`aion-rs` package): lifecycle, replay, timers, signals, queries, children, and supervision.
- **`crates/aion-nif`** — native-function declaration helpers for Gleam and Elixir workflows.
- **`crates/aion-proto`** — hand-written shared wire contracts.
- **`crates/aion-proto-generated`** — generated tonic/prost gRPC stubs.
- **`crates/aion-server`** — the HTTP/gRPC/WebSocket and worker-protocol server library; `aion server` runs it.
- **`crates/aion-darwin-acl`** — the macOS ACL decoder used by the server's path-safety gate.
- **`crates/aion-worker`** — the Rust remote-worker SDK.
- **`crates/aion-client`** — the Rust caller SDK.
- **`crates/aion-integrations`** — the neutral agent-harness integration contract and shared building blocks.
- **`crates/aion-integration-norn`** — the first-party Norn harness adapter.
- **`crates/aion-integration-cli`** — the plain-CLI harness adapter.
- **`crates/aion-cli`** — the `aion` binary for authoring, serving, deploying, and operating workflows.

Workflow authoring has two first-class surfaces: the typed Gleam SDK under `gleam/aion_flow/`, and AWL `.awl` documents. The AWL CLI verbs are `aion awl check`, `aion awl fmt`, `aion awl emit`, and `aion awl schema`; `aion deploy <file.awl>` direct-compiles and deploys a document, while `aion run <file.awl> --input <json>` compiles, deploys, starts, and awaits it. Python and TypeScript worker/client SDKs live under `sdks/python/` and `sdks/typescript/`, not under `crates/`.

beamr is reached through a single boundary module (`runtime`) inside the `aion` crate; no other module imports beamr.

## Load-Bearing Invariants

These are the architectural decisions every implementation and review must uphold. Violating one is a correctness bug, not a style nit.

1. **Type-erased events.** `Event` carries an opaque `Payload` (bytes + content-type tag), never a generic type parameter. The engine and store are type-erased; only the Gleam SDK knows concrete types. No `Event<T>`.
2. **The determinism boundary.** Workflow code must be deterministic and is re-executed on replay. Side effects must be recorded activities (the recorded result is returned on replay). `workflow.now` is the recorded event timestamp; `workflow.random` is seeded from `WorkflowId` + `RunId`. No wall clock, no entropy source in workflow-visible paths.
3. **Single writer per workflow.** Exactly one `Recorder` instance exists per active workflow and is the sole append path. Both the durability replay handoff (command-issued events) and the timer/signal/child services (asynchronous-arrival events) append through that one Recorder. **Never call `EventStore::append` directly.** A `SequenceConflict` signals a double-writer bug.
4. **Status is a projection.** `WorkflowStatus` (`Running`, `Completed`, `Failed`, `Cancelled`, `TimedOut`, `ContinuedAsNew`, `Paused`) is derived from event history, never a stored mutable field. `Paused` is non-terminal and is superseded by `WorkflowResumed`; engine-internal **residency** (`Resident` / `Suspended`) is separate and orthogonal — there is no `Suspended` status, and status reconciliation never touches residency.
5. **Content-hash module namespacing.** Each `.aion` package version is a distinct, immutable module named by its content hash (`logical_name$hash`, the `$` separator and SHA-256 are format constraints). This is how long-lived workflows coexist with new deploys without binding beamr's two-deep version limit.

## Coding Standards

This codebase runs mission-critical infrastructure for financial, legal, and healthcare settings.

- **No lazy code.** Every implementation complete and robust. No partial implementations, no deferred work, no "good enough for now."
- **No silent failures.** Every error handled, logged, or propagated. No swallowed `Result`s, no empty catch blocks, no bare `continue` on `Err`.
- **No shortcuts.** Handle all edge cases. Validate at boundaries. Test failure paths, not just happy paths.
- **No god files.** Nothing over 500 lines of code (excluding tests, comments, whitespace). Break it into modules.
- **Modular structure enforced.** `mod.rs` contains only `pub mod` declarations and re-exports. Logic goes in named files. `lib.rs`/`main.rs` are thin entry points.
- **Production ready.** All code deployable immediately. Would you trust this with patient records?
- **NO ARBITRARY LIMITS / NO ASSUMED DEFAULTS.** Don't add caps, rate limits, or hardcoded "sensible defaults" for configurable values (scheduler threads, timeouts, retry policy, poll intervals) — they come from the builder or are deferred to beamr's own default. Discuss values before implementing.
- **NO BACKWARDS COMPATIBILITY** during this build — no compat shims, no zombie code, no `#[deprecated]` markers. Replace, don't add alongside.

## Linting

Strict clippy lints in the workspace `Cargo.toml`: `unsafe_code = "deny"`, `missing_docs = "warn"`, pedantic enabled, warnings on `unwrap_used`/`expect_used`/`panic`/`todo`.

```
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Both must pass clean before any commit. If clippy fires, **fix the code**. `#[allow(...)]`, `#[expect(...)]`, `#[deny(...)]`, `#[ignore]` on tests, `_var` renames, and `#[cfg(any())]` dead-code hiding are all bypasses, not fixes. Tests that need a runtime gate it at runtime (read an env var, emit a `tracing::info!` skip line, return `Ok(())`) — never `#[ignore]`.

## Error Handling

- `thiserror` for library errors (domain-specific types). `anyhow` only in the binary (`aion-cli`, the unified `aion` executable) for top-level reporting.
- Never `.unwrap()` or `.expect()` in library code. Mutex/lock poison always handled explicitly and mapped to a typed error.

## Code Review

When work is ready, have it reviewed by a rigorous sub-agent on the **Opus** model — never a lighter model. Give the reviewer the brief, the original intent, and the relevant files, and let them explore beyond that. There is no such thing as a minor issue: everything is dealt with, nothing deferred, nothing skipped. **Standard:** would you trust this code with patient records, financial transactions, or legal documents? If not, it's not ready.

## The Brief Workflow

Implementation is driven by the per-cluster briefs under `docs/design/<cluster>/briefs/`. Each brief is a unit of work with numbered requirements (R1..Rn), EARS-style specs, concrete acceptance criteria, file paths, and checklist/story cross-references. Dispatch foundation-first: **AC → AP/AS → AE → AD/AT → AF/AN → AW/AR/AL/AU**. If you edit any brief JSON, re-render the markdown and re-run `check-coverage.py` (scripts under the meridian design-system) before landing. The brief files are authoritative; if a `design.json` `structure` annotation ever disagrees with a brief, trust the brief.
