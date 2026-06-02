---
type: design
cluster: aion-nif
title: Aion NIF — Native Helper for In-VM Activities and Deterministic Helpers
---

# Aion NIF — Native Helper for In-VM Activities and Deterministic Helpers

> Part of the **Aion** durable workflow engine. See
> `docs/design/workflow-engine/DESIGN-OVERVIEW.md` for the whole-system
> vision and `COMPONENT-ARCHITECTURE.md` for the crate map. This cluster is
> the `aion-nif` crate (COMPONENT-ARCHITECTURE "In-VM Activity Helpers").

## Intention

`aion-nif` is the crate an in-VM activity author reaches for. It makes
writing a native Rust function and exposing it to Gleam/Elixir workflows as
a NIF feel obvious and safe — typed arguments in, a typed value out, no raw
term arithmetic, no leaked heap pointers, no hand-rolled MFA registration.

It exists to serve the **Tier-2** band of the execution model
(DESIGN-OVERVIEW "Execution Tiers"): native code that runs inside the BEAM.
That band has two sharply different uses, and the entire crate is organised
around keeping them apart:

1. **Deterministic helpers** — JSON transformation, template rendering,
   parsing, formatting, hashing/crypto, math. Called *inline* from workflow
   code. Not recorded. Re-executed verbatim on replay. Safe precisely
   because the same inputs always produce the same output.
2. **Light in-VM activities** — small side-effectful operations (read a
   small file, run a quick command, read the system clock). These run on
   beamr's dirty scheduler and **must** be invoked through the engine's
   **activity contract** so their result is recorded once and returned from
   history on replay — never re-executed.

The crate must make the right thing the easy thing and the wrong thing hard
to express. A helper that does I/O but is wired as a deterministic inline
NIF is a determinism bug that corrupts replay. `aion-nif`'s declaration
surface is designed so that a side-effectful NIF is *typed* as an activity
from the first line, routed through AE's activity dispatch, and cannot be
called inline from workflow code by construction.

When this cluster is done, a host application can write a handful of native
functions, hand the resulting NIF set to `EngineBuilder::register_nifs`, and
have its Gleam workflows call them — deterministic ones inline, side-effectful
ones as recorded activities — with the determinism guarantee preserved.

## Problem

beamr exposes a raw NIF surface, demonstrated today in `beamr-meridian`:
a `NativeFn = fn(&[Term], &mut ProcessContext) -> Result<Term, Term>`
registered by module/function/arity into a `BifRegistryImpl`, with a
dirty-scheduler flag and an async-suspension facility (`request_suspend` +
`wake_with_result`). Writing one of these by hand is unpleasant and
error-prone in ways that matter for a workflow engine:

- **Term wrangling is manual and repetitive.** Every NIF re-implements
  `extract_string`, `extract_bytes`, `ok_binary`, `error_tuple`, list
  cons-cell building, and map decoding. The conversions are subtle (binary
  vs atom, cons list vs improper list) and a mistake is a runtime `badarg`,
  not a compile error.
- **Heap allocation for return values is a sharp edge.** `beamr-meridian`
  documents that BIFs cannot touch the process heap for some return shapes
  and falls back to `Box::leak`. That is an `unsafe`-adjacent, leak-prone
  pattern that every NIF author would otherwise have to rediscover and get
  right.
- **The deterministic/side-effectful distinction is invisible at the call
  site.** Nothing in the raw surface tells you whether a NIF is safe to
  call inline from deterministic workflow code or must be a recorded
  activity. The single most important invariant of the whole engine
  (DESIGN-OVERVIEW "The Core Concept: Determinism") is left to author
  discipline. A native NIF that runs a shell command is a *side effect*; if
  it is called inline it runs twice on replay and desynchronises history.
- **Registration is boilerplate.** Interning module and function atoms,
  matching arities, threading the registry — repeated verbatim per NIF, with
  the engine expecting a specific shape from `register_nifs`.

There is no shared, audited helper for any of this. `beamr-meridian` proved
the pattern works but did it ad hoc, inside one product. Aion needs a
reusable, general-purpose crate that any host can depend on, with the
determinism distinction baked into its types.

## Solution

One Rust crate, `aion-nif`, layered so each layer is independently
testable and the unsafe FFI surface is confined to a single audited module.

### The two-use split is the spine

Everything in the crate serves one of the two Tier-2 uses, and the API names
and types keep them apart:

- A **deterministic helper** is declared with the deterministic builder. It
  produces a `Nif` whose metadata marks it `Determinism::Pure`. The Gleam
  SDK (AF) binds it to an inline `@external` function. It is never recorded.
- A **side-effectful operation** is declared with the activity builder. It
  produces a `Nif` marked `Determinism::SideEffectful`, carries the
  dirty-scheduler flag, and is registered such that the only way a workflow
  invokes it is **through the engine's activity contract** (`activity.new`
  in AF, dispatched by AE's `activity` module). The crate provides the
  native body; AE wraps it so the result becomes an `ActivityCompleted`
  event and is returned from history on replay.

**Key decision — determinism is a typed property of every NIF, not a
convention.** Each declared NIF carries a `Determinism` tag in its
descriptor. The deterministic builder can only produce `Pure`; the activity
builder can only produce `SideEffectful`. The engine's registration path
(AE `register_nifs`) uses the tag: `Pure` NIFs are exposed for inline
binding; `SideEffectful` NIFs are exposed only as activity bodies. A
side-effectful NIF therefore *cannot* be wired as an inline deterministic
helper — the type system and the registration surface forbid it. Rejected:
a single untyped `nif!` macro with a doc note "don't call I/O ones inline" —
that leaves the engine's core invariant to author memory and review, exactly
the failure this crate exists to prevent.

### Layer 1 — Term conversion (`term` module)

A pair of traits over beamr's term API:

- `FromTerm` — fallibly decode a `Term` (with the `ProcessContext` for atom
  resolution) into a Rust type, yielding a typed `TermError` on mismatch
  rather than a bare `badarg`.
- `IntoTerm` — encode a Rust value into a `Term`, allocating on the process
  heap through the context.

Implemented for the primitives the design calls out: signed/unsigned
integers, floats, booleans, binaries (`Vec<u8>` / `String`), atoms (a small
`Atom`-name newtype), `Option<T>` (atom `nil` / value), `Result`-shaped
`{ok, T}` / `{error, E}` tuples, homogeneous lists (`Vec<T>`), and string-
keyed maps (`BTreeMap<String, T>` and decode into a struct via the JSON
bridge). The conversions build directly on beamr's `Binary`, `Tuple`,
`alloc_cons`, `alloc_tuple`, and the `term_to_value`/`value_to_term` JSON
helpers that `beamr-meridian` already uses.

### Layer 2 — Payload and JSON bridge (`payload` module)

Workflow values cross the boundary as `aion-core::Payload` (the type-erased
serialised value, AC-002). This module provides:

- `Payload` ↔ `Term` via the JSON content-type: decode a term to
  `serde_json::Value` (beamr's `term_to_value`) then into a `Payload`, and
  the reverse. This is how a NIF receives a workflow's activity input and
  returns its result in the shape AE records.
- `FromTerm`/`IntoTerm` blanket support for any `T: Serialize +
  DeserializeOwned` through `Payload`, so an author can write a NIF over
  ordinary Rust structs and get term marshalling for free.

This layer is where `aion-nif` depends on `aion-core` — for `Payload` and
for `ActivityError` (the error a side-effectful NIF returns).

### Layer 3 — NIF declaration (`declare` module)

Two ergonomic surfaces, both producing the same `Nif` descriptor:

- `deterministic_nif` — a builder/macro that takes a Rust `fn` (or closure)
  with `FromTerm` arguments and an `IntoTerm` return, generates the
  `NativeFn` shim (argument arity check, per-argument typed decode, body
  call, return encoding, uniform error mapping), and tags it
  `Determinism::Pure`. The author writes `fn(a: u64, b: String) -> Json`,
  not `fn(&[Term], &mut ProcessContext) -> Result<Term, Term>`.
- `activity_nif` — the same ergonomics, but the body returns
  `Result<T, ActivityError>`, the descriptor is tagged
  `Determinism::SideEffectful`, and it carries the dirty-scheduler flag (on
  by default for this kind, since side effects may block). The generated
  shim is the activity *body*; AE's `activity` module spawns it as the
  linked child process and records its outcome.

**Key decision — the macro generates the shim; the author never writes raw
term code.** The declaration surface owns arity checking, per-argument
decoding (with the failing argument's index in the error), heap-safe return
encoding, and panic-to-error containment. This removes the entire class of
hand-rolled term bugs and the `Box::leak` heap hazard from author code.
Rejected: a thin macro that still hands the author `&[Term]` — it would
leave the sharp edges exposed and defeat the crate's purpose.

### Layer 4 — Registration (`registry` module)

A `NifSet` builder collects declared `Nif`s under a module name and produces
the value `EngineBuilder::register_nifs` (AE) consumes. The set records, per
NIF, its module/function/arity, its `NativeFn`, its dirty flag, and its
`Determinism` tag. The engine's registration path reads the tags to decide
the binding mode (inline vs activity-body). `NifSet` validates at build time
that no `(module, function, arity)` triple is declared twice.

`aion-nif` does **not** call beamr's registry directly — it produces a
descriptor set; AE's `runtime` module (the sole beamr boundary, AE D1)
performs the actual `BifRegistryImpl::register`. This keeps the beamr import
confined to the engine and lets `aion-nif` stay a pure declaration library.

### The unsafe FFI seam (`raw` module)

Heap allocation for some return shapes touches a `beamr` facility that, today
in `beamr-meridian`, is handled with `Box::leak` because BIFs could not
access the process heap. Where `aion-nif` must use any `unsafe` to build
term values at the FFI boundary, it is confined to one module (`raw`),
behind safe wrappers, each `unsafe` block documented with the invariant it
upholds and why it is sound.

**Key decision — `unsafe` is isolated and justified at the FFI seam, not
blanket-denied.** CLAUDE.md mandates `unsafe_code = "deny"`. Where the beamr
term-construction boundary genuinely requires `unsafe` (or where beamr's safe
heap-allocation API is unavailable for a return shape), the crate sets
`unsafe_code = "deny"` at the crate root and grants a single, narrowly-scoped
exception **only in the `raw` module**, via a module-level
`#![allow(unsafe_code)]` *that is itself documented as the audited FFI seam*.
Every `unsafe` block inside carries a `// SAFETY:` comment stating the
invariant. The preferred outcome is **no `unsafe` at all**: if beamr exposes
(or is extended to expose, tracked as a beamr dependency) a safe process-heap
allocation API for every return shape `aion-nif` needs — `alloc_cons`,
`alloc_tuple` already exist on `ProcessContext` — then `raw` holds only safe
wrappers and the exception is removed entirely. The decision is therefore:
*prefer safe beamr APIs; if and only if a return shape has no safe path, the
unsafe lives in `raw`, documented and contained, never scattered and never
silently allowed elsewhere.* Rejected: a crate-wide `allow(unsafe_code)` —
it would let unaudited unsafe leak into conversion and declaration code where
it has no business being. Also rejected: forcing every NIF through `Box::leak`
as `beamr-meridian` does today — it leaks process memory per call and is not
production-acceptable for a long-running engine.

## Structure

```
crates/aion-nif/src/lib.rs          thin re-export surface
crates/aion-nif/src/term.rs         FromTerm / IntoTerm traits + primitive impls
crates/aion-nif/src/term_collection.rs  list + map + option + result term impls
crates/aion-nif/src/payload.rs      Payload <-> Term bridge; serde blanket via Payload
crates/aion-nif/src/declare.rs      deterministic_nif + activity_nif builders/macros
crates/aion-nif/src/descriptor.rs   Nif descriptor + Determinism tag + dirty flag
crates/aion-nif/src/registry.rs     NifSet builder; the value register_nifs consumes
crates/aion-nif/src/raw.rs          isolated, documented FFI seam (heap term build)
crates/aion-nif/src/error.rs        TermError, NifDeclError
```

## Constraints

- **CO1** — `unsafe_code = "deny"` at the crate root. Any `unsafe` is
  confined to `raw.rs` behind a single documented, audited module exception;
  every `unsafe` block carries a `// SAFETY:` comment. No `unsafe` outside
  `raw.rs`. The preferred state is zero `unsafe` (safe beamr APIs only).
- **CO2** — No `#[allow]` / `#[expect]` / `#[ignore]` lint bypasses anywhere,
  with the sole, explicit exception of the documented `#![allow(unsafe_code)]`
  FFI seam in `raw.rs` (CO1). No other bypass of any kind.
- **CO3** — `lib.rs` is declarations and re-exports only.
- **CO4** — 500-line file limit (excluding tests/comments/whitespace).
- **CO5** — `aion-nif` depends on `beamr` (term/NIF API) and `aion-core`
  (`Payload`, `ActivityError`) only. It does **not** depend on `aion` (the
  engine): it produces a descriptor set the engine consumes, it does not
  import the engine. This keeps an activity author who only writes NIFs from
  pulling the whole engine (COMPONENT-ARCHITECTURE open question — resolved
  "separate crate").
- **CO6** — Determinism is a typed property: the deterministic builder
  yields only `Determinism::Pure`; the activity builder yields only
  `Determinism::SideEffectful`. There is no untyped declaration path. A
  side-effectful NIF cannot be expressed as an inline deterministic helper.
- **CO7** — A side-effectful NIF's native body returns
  `Result<T, ActivityError>` (AC-004), so a failure carries the
  retryable/terminal classification AE/AT consult. It never returns a bare
  string error.
- **CO8** — Generated shims never expose raw `&[Term]` to author code, never
  `Box::leak` in author-reachable paths, and contain panics raised by the
  author body, converting them to a typed NIF error rather than unwinding
  across the FFI boundary.
- **CO9** — `NifSet` rejects a duplicate `(module, function, arity)` at build
  time with a typed `NifDeclError`, never silently overwrites.
- **CO10** — `aion-nif` registers nothing with beamr itself. It emits a
  descriptor set; AE's `runtime` module (the sole beamr boundary) performs
  the actual registration. `aion-nif` must not call `BifRegistryImpl::register`.

## Non-Goals

- **No engine, no activity recording, no retry logic.** AE owns activity
  dispatch and the `runtime` beamr boundary; AD owns event recording; AT owns
  retry decisions and durable waits. `aion-nif` provides the native bodies
  and the descriptor set those consume; it does not record events or decide
  retries.
- **No Gleam SDK / `@external` bindings.** AF (`aion_flow`) defines the Gleam
  functions that bind to these NIFs at runtime. `aion-nif` is the Rust side
  of that contract, not the Gleam side.
- **No remote / out-of-process workers.** Tier-3 workers in Python/TS/Rust
  are AR, a different mechanism over the wire protocol. NIFs are in-VM native
  code only.
- **No changes to beamr's NIF runtime.** This crate is a helper *over* the
  beamr surface (`NativeFn`, `BifRegistryImpl`, `request_suspend`,
  `wake_with_result`, the term API). Extending beamr (e.g. a safe heap-alloc
  API to eliminate the last `unsafe`) is beamr's own work, referenced here as
  a dependency, not done here.
- **No domain-specific NIFs.** This crate ships the *machinery* and a minimal
  illustrative set used only to prove the contract end-to-end. Meridian's
  concrete NIFs (run_step_norn, commit, …) live in the Meridian tree using
  this crate.
- **No async/await NIF model beyond beamr's suspend/wake.** Side-effectful
  NIFs that must not block a scheduler use beamr's dirty-scheduler flag and,
  where suspension is needed, the existing `request_suspend` /
  `wake_with_result` mechanism (as `beamr-meridian` demonstrates). The crate
  exposes that mechanism; it does not invent a new one.
