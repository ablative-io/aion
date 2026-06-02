---
type: design
cluster: aion-package
title: Aion Package — The .aion Workflow Archive Format
---

# Aion Package — The `.aion` Workflow Archive Format

> Part of the **Aion** durable workflow engine. See
> `docs/design/workflow-engine/DESIGN-OVERVIEW.md` for the whole-system
> vision and `COMPONENT-ARCHITECTURE.md` for the crate map.

## Intention

A workflow is deployed as a single file. This cluster defines that file —
the `.aion` package — and the `aion-package` crate that reads and writes
it. One file in, deployable workflow out: a `.aion` carries everything the
engine needs to load and run a workflow without scattered build artifacts,
side-channel metadata, or a recompilation step at the deployment boundary.

The format is the connective tissue between three concerns the rest of Aion
keeps coming back to. **Deployment** is copying one file. **Versioning** is
reading one field: the content hash of the compiled beams *is* the version,
computed once at pack time and verified on every load. **Hot code loading**
has a unit: the `.aion` is what gets hot-loaded, and the content-hash
naming scheme is what lets version N and version N+5 of the same workflow
coexist as distinct, immutable modules in beamr's registry.

When this cluster is done, the engine can be handed a path or a byte buffer
and get back a validated, integrity-checked, version-stamped package with
its beams ready to register — and an author (or the optional toolchain) can
take a set of compiled beams plus a manifest and produce a byte-identical,
reproducible `.aion`. The format must feel inevitable: an operator
inspecting a `.aion` with `unzip -l` should understand its layout at a
glance, and the manifest should read like a deployment descriptor, not an
internal dump.

## Problem

The engine needs a stable, self-describing input. Without a package format,
deploying a workflow means shipping a directory of `.beam` files, a
hand-maintained list of which module is the entry point, an out-of-band
record of the input and output shapes, and a separately tracked version
string — four things that drift apart the moment anyone touches them. The
engine would have to trust that the loose beams on disk are the ones the
version string refers to, with nothing to verify that claim.

Versioning is the sharpest edge. Aion workflows are long-lived: one may
sleep for three months and resume. beamr enforces a two-deep limit on
same-name module versions (a current and an old). A workflow that pins an
old version for months would, under naive same-name versioning, block every
subsequent deploy of that workflow — the third version has nowhere to go.
The resolution (settled in DESIGN-OVERVIEW.md) is to namespace each
deployed module by the content hash of the package, so each version is a
*distinct* module name rather than a same-name swap. That decision has to
be implemented *here*, in the format and the loader-facing surface, because
the content hash is computed and the namespaced names are derived at pack
and load time — not by the engine at run time.

Integrity is the third edge. A `.aion` travels: it is copied between hosts,
stored in registries, downloaded over networks. A truncated, tampered, or
bit-rotted package must be rejected at load, not discovered three steps into
registering half its modules into the VM. The content hash that doubles as
the version is also the integrity check, but only if the load path actually
recomputes and compares it rather than trusting the stored value.

This must be settled early: the engine's load path (cluster AE) consumes
`.aion` packages, and the indicative build order puts this format
immediately after the persistence contract and before the core engine.

## Solution

One crate, `aion-package`, depending only on `aion-core` (for the version
record type and identifier conventions) plus external crates. It owns the
`.aion` format end to end: the manifest model, the archive layout, the
content-hash versioning scheme, the write path, the read path, and the
integrity check.

A `.aion` file is a ZIP container (chosen for its ubiquitous tooling, random
access to entries, and per-entry compression) with a fixed internal layout:

```
package.aion
├── manifest.json        the deployment descriptor (see below)
├── beam/                compiled .beam modules
│   ├── <module>.beam    the workflow entry module
│   ├── <dep>.beam       its dependencies
│   └── <stdlib>.beam    the stdlib beams it needs
└── src/                 optional Gleam source (for inspection/recompile)
    └── <module>.gleam
```

### The Manifest

`manifest.json` is the package's self-description — a typed struct
(`Manifest`) serialised to JSON, never hand-edited at deploy time:

- **entry_module** — the BEAM module name of the workflow's entry point
  (the *logical* name, before content-hash namespacing).
- **entry_function** — the function to invoke to start the workflow.
- **input_schema** / **output_schema** — JSON-Schema descriptions of the
  workflow's input and result `Payload` shapes, so callers and the optional
  server-side authoring loop can validate before dispatch.
- **timeout** — the workflow's overall execution timeout.
- **activities** — the declared activity types the workflow invokes, so the
  engine knows up front which activity contracts must be satisfiable.
- **version** — the content hash (see below). Stored in the manifest *and*
  recomputed on load; the two must match.
- **format_version** — the `.aion` format's own schema version, so a future
  layout change is detectable rather than silently misread.

### Content-Hash Versioning

**Key decision — the content hash of the compiled beams IS the version.**
The version identifier is not a human-assigned string; it is a hash
computed over the set of `.beam` files (their names and bytes, in a
canonical order), independent of compression, timestamps, or source
inclusion. A new set of beams is a new version, automatically and
unforgeably. The hash is recorded in the manifest at pack time and
recomputed at load time; a mismatch is a rejected package. Rejected: a
human-assigned semantic version — it can lie, it can collide, and it
requires discipline the format should not depend on. The content hash
*cannot* lie about what is in the package.

**Key decision — the hash covers the beams, not the whole archive.** The
version must be stable across irrelevant variation: whether the source is
included, the ZIP compression level, file modification times in the
container, entry ordering on disk. So the hash is computed over a canonical
serialisation of the beam set alone (sorted by logical module name, each
contributing its name and its exact bytes), not over the archive file. Two
packages with identical beams but one with source and one without are the
*same version*. Rejected: hashing the archive bytes — it would make the
version depend on packaging incidentals and break reproducibility.

### Content-Hash Module Namespacing

**Key decision — deployed module names are namespaced by the content
hash.** When the engine loads a `.aion`, each module is registered under a
name derived from its logical name *and* the package's content hash (e.g.
`order_workflow$<hash>`), not its bare logical name. The package crate owns
the namespacing scheme: it provides the deterministic transformation from
(logical module name, content hash) to deployed module name, and the
inverse, so the engine and any tooling agree on the mapping. Consequences,
each load-bearing:

- Version N and version N+5 coexist as separate, immutable modules — no
  name conflict, no swap.
- beamr's two-deep same-name version limit is sidestepped *entirely* for
  workflow modules: each is a distinct name, so the limit never binds.
- Replay is safe by construction: an in-flight execution names its modules
  by the hash it started on, and that module set never mutates.

This is application-level versioning layered *on top of* beamr's VM-level
dual-version hot-loading, which still governs shared/stdlib modules and the
engine's own modules. The package crate does not *perform* the loading —
that is the engine (cluster AE) — but it owns the *naming scheme* the
engine applies, because the names are a function of the content hash this
crate computes.

### Write Path — Producing a `.aion`

`PackageBuilder` takes a manifest (minus the version, which it computes), a
set of named beam modules, and optional source, and writes a `.aion`:

1. Canonicalise the beam set and compute the content hash.
2. Stamp the hash into the manifest as the version.
3. Serialise the manifest, lay out the ZIP entries in canonical order, and
   write the archive — deterministically, so the same inputs produce a
   byte-identical archive (reproducible builds).

### Read Path — Loading a `.aion`

`Package::load` (from a path or a byte buffer) reverses it:

1. Open the ZIP, read and parse `manifest.json`, check `format_version`.
2. Extract the beam set.
3. Recompute the content hash over the beams and compare it to the
   manifest's `version`. Mismatch → `IntegrityError`, package rejected.
4. Expose a validated `Package`: the manifest, the beam set (as logical
   name → bytes), the optional source, and the derived namespaced module
   names ready for the engine to register.

The read path *never* registers anything into a VM and *never* touches the
event store directly — it produces an in-memory, validated value. Loading
is total: every failure mode (not a ZIP, missing manifest, unknown format
version, missing entry module, hash mismatch, malformed beam entry) maps to
a distinct, typed `PackageError`.

### Version Record

`aion-core` defines the version record type used across the system. This
crate produces it from a loaded package (logical entry module, content hash,
declared activities, schemas) so the engine and store reference a workflow
version through one shared type rather than re-deriving it.

## Structure

```
crates/aion-package/src/lib.rs          thin re-export surface
crates/aion-package/src/manifest.rs     Manifest struct + format_version + serde
crates/aion-package/src/hash.rs         content-hash computation over the beam set
crates/aion-package/src/namespace.rs    logical name <-> namespaced name scheme
crates/aion-package/src/beam.rs         BeamModule + BeamSet (canonical ordering)
crates/aion-package/src/builder.rs      PackageBuilder — the write path
crates/aion-package/src/package.rs      Package + load (read path) + integrity check
crates/aion-package/src/error.rs        PackageError taxonomy
crates/aion-package/src/version.rs      version-record production from a Package
```

## Constraints

- **CO1** — `unsafe_code = "deny"`. No unsafe in the crate.
- **CO2** — No `#[allow]` / `#[expect]` / `#[ignore]` lint bypasses per
  CLAUDE.md.
- **CO3** — `lib.rs` / `mod.rs` are declarations and re-exports only.
- **CO4** — 500-line file limit (excluding tests/comments/whitespace).
- **CO5** — `aion-package` depends only on `aion-core` among Aion crates.
  It must not depend on `aion`, `aion-server`, beamr, or any storage
  backend. Structural; must hold.
- **CO6** — All library errors are `thiserror` enums; no `anyhow` in this
  library crate. No `.unwrap()` / `.expect()` outside tests.
- **CO7** — The content hash is computed over the canonical beam set only,
  never over archive bytes, compression, timestamps, or source inclusion.
  Two packages with identical beams have identical versions.
- **CO8** — The write path is deterministic: identical inputs produce a
  byte-identical `.aion`. No wall-clock timestamps or nondeterministic
  ordering may leak into the archive.
- **CO9** — The read path recomputes the content hash and rejects any
  package whose recomputed hash does not equal the manifest's stored
  version. Integrity is verified, never trusted.
- **CO10** — Loading is total: every malformed-input case maps to a
  distinct typed `PackageError` variant; no panic, no silent fallback, no
  partial result on error.
- **CO11** — The crate performs no VM loading and no I/O against the event
  store. It produces validated in-memory values; the engine (AE) does the
  registering.
- **CO12** — The namespacing transform is a pure, total bijection between
  (logical module name, content hash) and the deployed module name, with a
  verified round-trip.

## Non-Goals

- No loading modules into beamr and running them — that is the engine
  (cluster AE). This crate produces the namespaced names; the engine applies
  them.
- No compiling Gleam source to beams — that is the optional `aion-toolchain`
  (a separate cluster). This crate packages already-compiled beams and may
  carry source for inspection, but never invokes a compiler.
- No event store or persistence — clusters AC (in-memory) / AS (libSQL) /
  AX (Postgres). The version record produced here is *referenced* by the
  store; this crate does not write it.
- No network transport or registry protocol for shipping `.aion` files —
  that is server/client concern (cluster AW and beyond).
- No hot-code-loading mechanism itself — that is beamr plus the engine. This
  crate defines the *unit* of hot-load (the package) and the naming scheme
  that makes it work, not the load operation.
- No signing or encryption of packages — integrity here is a content hash,
  not authenticity. Cryptographic signing is a later, separate concern.
