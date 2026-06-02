# CLAUDE.md

## What This Is

Aion is a general-purpose **durable workflow engine** — Temporal-class durability built on Gleam, Rust, and the BEAM, running on the **beamr** VM (our Rust BEAM implementation, a crates.io dependency). Durable execution means a workflow can crash on step nine of ten and resume exactly where it was, or sleep for three months and wake in the same logical state. The mechanism is **event-sourcing plus deterministic replay**: workflow code is re-executed from the start on recovery, and every side-effecting call returns its recorded result from history instead of acting again.

Aion is general purpose. Meridian is the first consumer, not a constraint.

The design lives under `docs/design/` — twelve clusters, all reviewed and approved. JSON is the source of truth (`design.json`, `checklist.json`, `stories.json`, `briefs/*.json`); markdown is rendered output. The whole-system picture is in `docs/design/workflow-engine/DESIGN-OVERVIEW.md` and `COMPONENT-ARCHITECTURE.md` — read those first.

## Architecture

The crate / package family:

- **`aion-core`** — pure domain model: `Event` enum, `Payload`, newtype identifiers, `WorkflowStatus`, filters, error taxonomy. Leaf crate.
- **`aion-store`** — the `EventStore` persistence trait, `StoreError`, `InMemoryStore` reference, and the conformance suite (the behavioural oracle every backend must match). Leaf (depends only on `aion-core`).
- **`aion-store-libsql`** — the default durable `EventStore` over libSQL; runs the conformance suite.
- **`aion-package`** — the `.aion` archive format, content-hash versioning, module namespacing, and the `WorkflowVersion` record.
- **`aion`** — the engine. Embeds beamr; owns workflow lifecycle, process-per-workflow management, supervision, `.aion` loading (cluster AE), durability and replay (the `durability` module set, AD), and timers/signals/queries/children/concurrency (the `time`/`signal`/`query`/`child`/`concurrency` modules, AT). Transport-agnostic.
- **`aion-nif`** — Rust helper for writing and registering the NIFs Gleam activities call.
- **`aion-proto`** / **`aion-server`** — the wire contract and the standalone deployable (HTTP/gRPC/WebSocket, worker protocol, multi-tenancy).
- **`aion-worker[-python/-typescript]`**, **`aion-client[-python/-typescript]`** — remote worker and caller SDKs.
- **`aion_flow`** (Gleam, Hex) — the typed authoring SDK. **`aion-dashboard`** — the React monitoring UI.

beamr is reached through a single boundary module (`runtime`) inside the `aion` crate; no other module imports beamr.

## Load-Bearing Invariants

These are the architectural decisions every implementation and review must uphold. Violating one is a correctness bug, not a style nit.

1. **Type-erased events.** `Event` carries an opaque `Payload` (bytes + content-type tag), never a generic type parameter. The engine and store are type-erased; only the Gleam SDK knows concrete types. No `Event<T>`.
2. **The determinism boundary.** Workflow code must be deterministic and is re-executed on replay. Side effects must be recorded activities (the recorded result is returned on replay). `workflow.now` is the recorded event timestamp; `workflow.random` is seeded from `WorkflowId` + `RunId`. No wall clock, no entropy source in workflow-visible paths.
3. **Single writer per workflow.** Exactly one `Recorder` instance exists per active workflow and is the sole append path. Both the durability replay handoff (command-issued events) and the timer/signal/child services (asynchronous-arrival events) append through that one Recorder. **Never call `EventStore::append` directly.** A `SequenceConflict` signals a double-writer bug.
4. **Status is a projection.** `WorkflowStatus` (Running, Completed, Failed, Cancelled, TimedOut) is derived from event history, never a stored mutable field. Each terminal status has exactly one corresponding terminal event. Suspension is a separate engine-internal **residency** flag (Resident / Suspended), orthogonal to status — there is no `Suspended` status, and status reconciliation never touches residency.
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

- `thiserror` for library errors (domain-specific types). `anyhow` only in the binary (`aion-server`) for top-level reporting.
- Never `.unwrap()` or `.expect()` in library code. Mutex/lock poison always handled explicitly and mapped to a typed error.

## Code Review

When work is ready, have it reviewed by a rigorous sub-agent on the **Opus** model — never a lighter model. Give the reviewer the brief, the original intent, and the relevant files, and let them explore beyond that. There is no such thing as a minor issue: everything is dealt with, nothing deferred, nothing skipped. **Standard:** would you trust this code with patient records, financial transactions, or legal documents? If not, it's not ready.

## The Brief Workflow

Implementation is driven by the per-cluster briefs under `docs/design/<cluster>/briefs/`. Each brief is a unit of work with numbered requirements (R1..Rn), EARS-style specs, concrete acceptance criteria, file paths, and checklist/story cross-references. Dispatch foundation-first: **AC → AP/AS → AE → AD/AT → AF/AN → AW/AR/AL/AU**. If you edit any brief JSON, re-render the markdown and re-run `check-coverage.py` (scripts under the meridian design-system) before landing. The brief files are authoritative; if a `design.json` `structure` annotation ever disagrees with a brief, trust the brief.
