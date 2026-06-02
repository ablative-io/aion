# Aion-Nif — User Stories

## In-VM Activity Author — Writing Native Rust NIFs for Workflows

**S1.** As an in-VM activity author, I want to declare a native function with ordinary typed Rust arguments and return value so that I never hand-write term decoding or encoding.

**S2.** As an in-VM activity author, I want to write a NIF directly over my own Serialize/Deserialize structs so that workflow values marshal across the boundary without manual JSON glue.

**S3.** As an in-VM activity author, I want a panic in my NIF body to surface as a typed error rather than crash the VM so that a bug in one helper does not take down the scheduler.

**S4.** As an in-VM activity author writing a side-effectful operation, I want the API to force it to be an activity returning an ActivityError so that I cannot accidentally make it an inline helper that breaks replay.

## Workflow Author — Calling NIFs from Gleam Workflow Code

**S5.** As a workflow author, I want deterministic NIF helpers to be callable inline from my workflow so that pure transforms (JSON, templating, formatting) cost nothing extra and replay safely.

**S6.** As a workflow author, I want any side-effectful NIF to go through the activity contract so that its result is recorded once and returned from history on replay instead of running twice.

## Engine Operator — Registering NIFs with the Engine

**S7.** As an engine operator, I want to hand a single NIF set to register_nifs so that wiring native helpers into the engine is one call, not per-function boilerplate.

**S8.** As an engine operator, I want a duplicate NIF declaration to be rejected at build time so that a registration clash is caught before the engine runs rather than silently overwriting a function.

## Reviewer — Auditing the Native Boundary

**S9.** As a reviewer, I want all unsafe confined to one documented FFI module with SAFETY comments so that I can audit the entire native-boundary risk surface in one place.

**S10.** As a reviewer, I want the deterministic-vs-side-effectful distinction enforced by the type system so that I can trust the determinism invariant without reading every call site.
