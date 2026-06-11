# Design Brief — Workflow Packaging Tooling (`workflow.toml` → `package_project` → `aion-cli package`)

**Repo:** `/Users/tom/Developer/ablative/aion`, main @ `1769b710` ("Point package metadata at the canonical repo and author").
**Coordination warning:** another agent is actively editing `crates/aion/src/**` (#58 Wave C — working tree dirty in `engine/`, `runtime/`). This work touches **only** `crates/aion-package`, `crates/aion-cli`, `examples/`, and docs — a disjoint file set. No `git stash`; commit verified waves immediately (hard project rule). Task **#52** (CLI error-detail improvements) touches `aion-cli` error rendering — Wave 2 must rebase on it or coordinate.

This brief is self-contained: an implementing agent needs no prior conversation context.

## DECISIONS — SIGNED OFF (Tom, 2026-06-11 — all twelve adopted)

- **D-1 → (A).** Schemas as paths to JSON files, required.
- **D-2 → (A).** `timeout_seconds` integer ≥ 1, required.
- **D-3 → (A).** `entry_function` required, no `"run"` default.
- **D-4 → (A).** Activities as a flat list of strings.
- **D-5 → (B), as recommended.** Source included by default (`package.include_source = true`, all first-party `src/**/*.gleam`); opt-out flag for proprietary-source deployments. Hash-neutral.
- **D-6 → (A).** `[[workflow]]` array of tables from day one; one `.aion` per entry.
- **D-7 → (B).** `output` optional; derived `<entry_module>.aion` otherwise.
- **D-8 → (B).** `build/dev/erlang` + production-dependency-closure filter from `gleam.toml`/`manifest.toml`.
- **D-9 → (A).** Hardcoded SDK test-module filter only, scoped to `aion_flow`'s ebin; reported, never silent for user modules.
- **D-10 → (B), RESOLVED BY RENAME.** The workflow project config file is **`workflow.toml`**, not `aion.toml` — eliminating the collision with aion-server's auto-discovered `aion.toml` server config (that file keeps its name). All references in this brief are updated accordingly; R7's `deny_unknown_fields` stays as defence in depth.
- **D-11 → (A).** No-build default; `--build` opt-in.
- **D-12 → (A).** JSON result document on stdout, house style.

## Commissioned shape (Tom, 2026-06-11 — fixed constraints, design within them)

1. **Library-first:** a public project-packaging API in `aion-package` (e.g. `package_project(root, options)`) consuming an **already-built** Gleam project. **No process spawning in the library** — Meridian links this function directly and exposes it as a native agent tool in its unified CLI binary.
2. `aion-cli package` is a thin shell over that library; process spawning (optionally invoking `gleam build`) is allowed in the CLI layer only.
3. Manifest configuration comes from a `workflow.toml` in the workflow project root (commissioned as `aion.toml`; renamed per the D-10 resolution).
4. The bespoke example packager binaries are **replaced** by the new tool; the examples become consumers and the proof.
5. End-user documentation: how to package an arbitrary Gleam workflow.

---

## 1. Verified current-state map

### 1.1 `aion-package` public API (complete, shipped)

- `Manifest` (`crates/aion-package/src/manifest.rs:51-78`): `entry_module: String`, `entry_function: String`, `input_schema: serde_json::Value` (JSON-Schema document), `output_schema: serde_json::Value`, `timeout: Duration` (serde-encoded `{secs, nanos}`), `activities: Vec<DeclaredActivity>`, `version: ManifestVersion`, `format_version: u32`. **`DeclaredActivity` carries only `activity_type: String`** (`manifest.rs:36-43`) — no timeouts, no retry policy. Those are call-site activity config and worker concerns, **not** manifest fields; adding them is a `format_version` bump and is out of scope here.
- `BeamSet::new` (`beam.rs:63-83`): canonical name-sorted order, rejects duplicates (`MalformedBeamEntry`) and `RESERVED_MODULE_NAMES = ["aion_flow_ffi"]` (`ReservedModuleName`).
- `PackageBuilder::{new, with_source}` → `finalise_manifest` / `write_to_bytes` / `write_to_path` (`builder.rs`): stamps `manifest.version` with the beam content hash, rejects manifests whose entry module is absent from the beam set, writes a deterministic ZIP (Stored compression, `DateTime::DEFAULT` timestamps, 0644, canonical entry order: `manifest.json`, `beam/<name>.beam` sorted, `src/<name>.gleam` sorted via `BTreeMap`). `is_safe_logical_name` (`builder.rs:150-159`) rejects empty components, `.`/`..`, backslashes, leading separators, and the `$` deployed-name separator.
- `content_hash` (`hash.rs`): SHA-256 over logical names + exact beam bytes in canonical order. **Source inclusion, archive bytes, and compression never affect the hash** (documented at `hash.rs:13-24`; pinned by builder test `source_inclusion_does_not_change_manifest_version`).
- `Package::load_from_path/load_from_bytes` (`package.rs`): parse manifest → format-version check → extract beams/source → recompute hash → `IntegrityMismatch` on disagreement → `MissingEntryModule` check. Exposes `version_record() -> WorkflowVersion`, `deployed_modules()`, `deployed_entry_module()`.
- `PackageError` (`error.rs`): `ArchiveRead`, `MissingManifest`, `ReservedModuleName`, `ArchiveWrite`, `ArchiveWriteIo`, `ManifestParse`, `ManifestSerialise`, `UnknownFormatVersion`, `MissingEntryModule`, `IntegrityMismatch`, `MalformedBeamEntry`.
- **The workflow type IS the entry module**: `crates/aion/src/loader/load.rs:325` sets `workflow_type: manifest.entry_module.clone()`. There is no separate "workflow type name" to configure — `workflow.toml` must not invent one.
- Crate deps (`Cargo.toml`): `aion-core`, `serde`, `serde_json`, `thiserror`, `sha2`, `zip`. CO5 ("depends only on `aion-core` among Aion crates") permits adding the external `toml` crate.

### 1.2 The seven bespoke packagers (what varies — this is the `workflow.toml` schema)

`examples/{hello-world,approval-gate,data-pipeline,order-saga,batch-orchestrator,subscription,agent-orchestration}/packager/src/main.rs` — **seven**, not four; all are deleted by this work. They are line-for-line identical except:

| Varies | Examples of values |
|---|---|
| Entry module | `hello_world`, `orchestrator` (≠ directory name), … |
| Entry function | `run` in all seven (convention, not contract) |
| Output filename | `hello-world.aion`, `orchestrator.aion` (≠ `<entry_module>.aion` in two cases) |
| Input/output schemas | inline `json!` documents, 5–60 lines each (order-saga's output schema is a 40-line `oneOf`) |
| Timeout | 30 s, 3600 s, 31 536 000 s (one year) |
| Declared activities | 1–6 plain `activity_type` strings |
| Included source | exactly one file: the entry module's `src/<entry>.gleam` |

Invariant across all seven: read `build/dev/erlang/*/ebin/*.beam` (skip dirs without `ebin`, skip non-`.beam` like `aion_flow.app`), module name = file stem, exclude test-only stems (`aion_flow_ffi`, `aion@testing`, `aion@testing@*` — doc comment at `hello-world/packager/src/main.rs:106-113`), `bail!` if the entry module is missing, `PackageBuilder::with_source(...).write_to_path(...)`, `println!` progress. The packagers are standalone crates (not workspace members), invoked via `cargo run --manifest-path`.

### 1.3 Gleam build output (verified against `examples/hello-world/build/`)

- `gleam build` compiles the root project **and all dependencies** to `build/dev/erlang/<package>/ebin/*.beam`. hello-world's tree: `aion_flow`, `aion_hello_world`, `gleam_json`, `gleam_stdlib`, plus a non-package `fingerprint` dir (no `ebin`, already skipped).
- Nested Gleam modules compile to `@`-joined stems (`aion@workflow@run.beam`) — these are the logical module names; `is_safe_logical_name` accepts `@`.
- `aion_flow`'s **production** ebin contains test machinery: `aion_flow_ffi.beam` (the SDK's in-process engine double — reserved namespace) and `aion@testing*.beam`. The filter is mandatory, not cosmetic: an unfiltered `BeamSet::new` fails on `aion_flow_ffi`.
- `gleam build` has no prod profile; dev-dependencies of the **root** project (e.g. `gleeunit`) also land under `build/dev/erlang/`. The examples have no dev-deps, but arbitrary user projects will (see D-8).
- Gleam's lockfile is **`manifest.toml`** in the project root (name collision with our manifest concept only, not with `workflow.toml`); `gleam.toml` carries `[dependencies]` / `[dev-dependencies]` and the package `name`.

### 1.4 `aion-cli` (the shell)

`crates/aion-cli/src/{main,output,payload}.rs`: clap derive, global `--endpoint/--namespace/--subject/--pretty`, six remote subcommands. **`main` unconditionally builds a gRPC client before dispatch (`main.rs:111-117`)** — a local `package` command must not connect; client construction has to move into the remote-command path. Error style: `anyhow` with `.context(...)`, errors print as `Error: <chain>` and exit 1; results print as compact JSON (pretty with `--pretty`) via `output::print_json`. Task #52 is improving error detail in this crate.

### 1.5 The `aion.toml` name is already taken — by the server (resolved by the D-10 rename)

`aion-server` **auto-discovers a file named `aion.toml` in its working directory as server config** when `--config` is omitted (GETTING-STARTED.md "Config auto-discovery"; users are told to `cp dev-config.toml aion.toml` at the repo root). That file keeps its name. The workflow packaging descriptor is named **`workflow.toml`** instead, eliminating the footgun outright (D-10, resolved 2026-06-11).

### 1.6 Design anticipation

`docs/design/aion-package/DESIGN.md:29-36` — "an author (or the **optional toolchain**) can take a set of compiled beams plus a manifest and produce a byte-identical, reproducible `.aion`". This brief is that toolchain. The format, hash semantics, and `PackageBuilder` write path are unchanged; everything here layers on top.

---

## 2. `workflow.toml` schema

Full schema (TOML, `serde(deny_unknown_fields)` on every table — unknown keys are hard errors, R7):

```toml
# workflow.toml — workflow packaging descriptor, lives in the Gleam project root
# (next to gleam.toml).

[package]                      # table optional; all fields below optional
include_source = true          # D-5: default true (argued), first-party src only

[[workflow]]                   # D-6: array of tables; one .aion per entry
entry_module   = "hello_world"           # REQUIRED — also the workflow type (§1.1)
entry_function = "run"                   # REQUIRED — D-3: no "run" default
timeout_seconds = 30                     # REQUIRED — D-2: integer seconds ≥ 1
input_schema   = "schemas/input.json"    # REQUIRED — D-1: path relative to root
output_schema  = "schemas/output.json"   # REQUIRED — D-1
activities     = ["greet"]               # REQUIRED — D-4: may be [], not omitted
output         = "hello-world.aion"      # OPTIONAL — D-7: derived default
```

Field-level requirements:

- **R1** — `entry_module`: required, non-empty, must satisfy `is_safe_logical_name` (no `$`, no path tricks); unique across all `[[workflow]]` entries.
- **R2** — `entry_function`: required, non-empty.
- **R3** — `timeout_seconds`: required, integer ≥ 1 (a zero workflow timeout is meaningless and rejected at parse).
- **R4** — `input_schema` / `output_schema`: required paths, resolved against the project root, file must exist and parse as JSON. The parsed `serde_json::Value` goes into the manifest verbatim. No meta-schema validation of the document (out of scope, §11).
- **R5** — `activities`: required list of non-empty, unique strings (the `activity_type` keys). An explicitly empty list is legal (workflows without activities exist); **omission** is an error — the author must state "this workflow declares no activities" rather than forget the field.
- **R6** — at least one `[[workflow]]` entry; `output` values (explicit or derived) must be pairwise distinct.
- **R7** — unknown keys anywhere → typed error naming the key and table. This also backstops the (rename-resolved) D-10 concern as defence in depth: any misplaced config file read as a packaging descriptor fails loudly on its first field.

### DECISION D-1 — schema representation

- **(A) Paths to JSON files** (`input_schema = "schemas/input.json"`), required.
- (B) Inline JSON strings in TOML.
- (C) Inline TOML tables mapped structurally to JSON.

**ADOPTED (Tom, 2026-06-11): A, as recommended.**

**Recommendation: A.** Five of the seven real schemas are 15–60-line documents (order-saga's `oneOf` is unreadable as an escaped TOML string); JSON files get editor/validator tooling and are reusable by workers and tests. (C) is rejected outright: TOML cannot represent JSON `null` (legal in `enum`/`const` schemas) and the mapping invites subtle lossiness. One representation, no fallback — a `{"type":"string"}` schema is a one-line file.

### DECISION D-2 — timeout representation

- **(A) `timeout_seconds` integer.**
- (B) Humantime-style duration string (`timeout = "1h"`).

**ADOPTED (Tom, 2026-06-11): A, as recommended.**

**Recommendation: A.** Consistent with the server-config key style (`query_timeout_ms`, `AION_DRAIN_TIMEOUT_SECONDS` — unit in the key name), zero new dependencies, and every shipped example uses whole seconds. Sub-second workflow timeouts are not a real case; `Duration::from_secs` maps losslessly into the manifest's `Duration`.

### DECISION D-3 — `entry_function` default

- **(A) Required, no default.**
- (B) Default `"run"`.

**ADOPTED (Tom, 2026-06-11): A, as recommended.**

**Recommendation: A.** All seven examples use `run`, but that is SDK convention, not format contract; defaulting it is exactly the "assumed default" CLAUDE.md bans. One line in `workflow.toml` buys an explicit deployment descriptor.

### DECISION D-4 — activity declaration shape

- **(A) Flat list of strings** (`activities = ["greet"]`).
- (B) Array of tables (`[[workflow.activity]] type = "greet"`) reserving room for future fields.

**ADOPTED (Tom, 2026-06-11): A, as recommended.**

**Recommendation: A.** The manifest's `DeclaredActivity` carries only `activity_type` (§1.1); per-activity timeouts/retry policy are not manifest fields today and adding them is a `format_version` bump with its own brief. Shaping config for fields that cannot be expressed is speculative structure. If the manifest grows fields, the TOML value can become `string | table` then — a backwards-compatible widening (and pre-1.0 we don't owe compatibility anyway).

### DECISION D-5 — source inclusion

- (A) Required boolean, author must choose.
- **(B) Optional `package.include_source`, default `true`, first-party sources only.**
- (C) Default `false`.

**ADOPTED (Tom, 2026-06-11): B, as recommended — source included by default, opt-out flag.**

**Recommendation: B.** These packages are deployed artifacts for financial/legal/healthcare settings: when an operator inspects a running version months later, the first-party `.gleam` source is the only provenance that isn't recoverable from Hex. The default is argued, not arbitrary: inclusion is hash-neutral (§1.1 — pinned by an existing builder test, so version identity never depends on this flag), costs only archive bytes, and auditability-by-default is the correct posture for this codebase. Scope: **all `src/**/*.gleam` of the root project only** (keyed by module path relative to `src/`, e.g. `aion/util.gleam` → logical `aion/util`) — dependency sources are recoverable from their registries; the current packagers' entry-module-only inclusion was an accident of hand-rolling, and partial source is worse than none for audit. Opt-out exists for proprietary-source deployments.

### DECISION D-6 — multi-workflow projects

- **(A) `[[workflow]]` array of tables from day one; one `.aion` per entry, all sharing the project's beam set.**
- (B) Single `[workflow]` table; multi-workflow is future work.

**ADOPTED (Tom, 2026-06-11): A, as recommended.**

**Recommendation: A.** One package = one entry module is a format constraint (`Manifest` has a single entry), so multi-workflow necessarily means N packages. Supporting N≥1 is a loop over manifest construction against one discovered `BeamSet` — nearly free in implementation — whereas `[workflow]` → `[[workflow]]` later is a breaking schema change for every existing file. Note the consequence and test it: all packages from one project share the same content hash (it covers beams only), which is correct — `WorkflowVersion` is (entry module, hash) and deployed entry names `entry$hash` remain distinct.

### DECISION D-7 — output naming

- (A) `output` required per workflow.
- **(B) `output` optional; when absent, derived as `<entry_module>.aion` in the project root.**

**ADOPTED (Tom, 2026-06-11): B, as recommended.**

**Recommendation: B.** The derived value is a pure function of an already-required field — a derivation, not an assumed default. Two examples (`hello-world.aion`, `orchestrator.aion` with entry `orchestrator`... the former) keep their published names via the explicit field. Relative `output` paths resolve against the project root; the CLI `--out` overrides (single-workflow projects only, error otherwise — see §5).

---

## 3. Module discovery + filtering

- **R8** — Discovery root is `<project>/build/dev/erlang`. The library never builds; if the directory (or any required package's `ebin`) is missing, fail with `ProjectNotBuilt` whose message names the missing path and says to run `gleam build` (or `aion-cli package --build`).
- **R9** — Per-package collection: for each package in the **production dependency closure** (D-8), read `build/dev/erlang/<pkg>/ebin/*.beam`; module name = UTF-8 file stem; non-UTF-8 stems are typed errors; non-`.beam` entries (`.app`, `fingerprint/`…) are skipped silently.
- **R10** — SDK test-module filter (D-9): stems `aion_flow_ffi`, `aion@testing`, and prefix `aion@testing@` are excluded **only when found in the `aion_flow` package's ebin**, and each exclusion is recorded in the returned report (module, package, reason). A *user's own* module named `aion_flow_ffi` must NOT be silently dropped — it flows into `BeamSet::new` and fails typed as `ReservedModuleName` (the existing `RESERVED_MODULE_NAMES` contract is the single source of truth; the library adds no second list).
- **R11** — Duplicate logical module names across packages are detected **before** `BeamSet::new` so the error can carry provenance (both package names), instead of the bare `MalformedBeamEntry` the set would give.
- **R12** — Entry-module presence is checked per workflow against the discovered set before building, producing a friendlier `EntryModuleNotFound { module, searched }` than the builder's late `MissingEntryModule`.

### DECISION D-8 — build profile and dependency filtering

- (A) Read `build/dev/erlang/*/ebin` wholesale (today's packager behaviour). Dev-dependencies of the root project (e.g. `gleeunit`) ship inside deployed packages.
- **(B) Read `build/dev/erlang` but include only the production dependency closure**, computed from `gleam.toml` (`name` + `[dependencies]`) transitively closed over the `manifest.toml` lockfile's per-package `requirements`.
- (C) Require `gleam export erlang-shipment` (prod-compiled tree).

**ADOPTED (Tom, 2026-06-11): B, as recommended.**

**Recommendation: B.** `gleam build` is the only plain build command and the documented user flow (A's directory, C's rejected: shipment is an OTP-release layout produced by a different command the library can't run, and forcing it complicates the "build then package" loop). But (A) ships test frameworks inside healthcare-grade deploy artifacts the moment a user adds a dev-dependency — silent payload bloat at best, reserved-name collisions at worst. The closure computation is two small TOML reads against files Gleam guarantees exist for a built project (missing/unparseable lockfile is a typed error), and it is deterministic. Closure exclusions are recorded in the report like filter exclusions.

### DECISION D-9 — test-module filter configurability

- **(A) Hardcoded SDK filter only** (`aion_flow_ffi`, `aion@testing`, `aion@testing@*`, scoped to the `aion_flow` package per R10).
- (B) Additionally a user-configurable `exclude_modules` list in `workflow.toml`.

**ADOPTED (Tom, 2026-06-11): A, as recommended.**

**Recommendation: A.** The filtered names are aion-SDK-owned internals whose exclusion is a correctness requirement, not a preference — they belong in the library next to `RESERVED_MODULE_NAMES`, versioned with the SDK that produces them. No example or known consumer needs user-level excludes; (B) is speculative config surface and a foot-gun (excluding a module another module calls produces a runtime `undef` deep inside a deployed workflow). Revisit only against a concrete need.

### DECISION D-10 — the `aion.toml` filename collision

- (A) Keep `aion.toml` (commissioned name) with the R7 `deny_unknown_fields` mitigation, plus an explicit callout in docs that the server's auto-discovered config file happens to share the name.
- **(B) Rename to `workflow.toml`.**

**ADOPTED (Tom, 2026-06-11): B — RESOLVED by renaming the workflow project config file to `workflow.toml`.** The original recommendation was (A) flagged for Tom's attention, because GETTING-STARTED literally instructs users to create a repo-root `aion.toml` for the *server* and "now is the only cheap time" to rename; Tom took the rename. aion-server's auto-discovered `aion.toml` server config keeps its name and is untouched — the ambiguity is eliminated outright rather than mitigated. R7's `deny_unknown_fields` stays as defence in depth, and every reference to the packaging descriptor in this brief now reads `workflow.toml`.

---

## 4. Library API surface (`crates/aion-package`)

New module tree (house rule: ≤500 code lines per file, `mod.rs` is declarations only):

```
crates/aion-package/src/project/mod.rs        pub mod decls + re-exports
crates/aion-package/src/project/config.rs     workflow.toml types, parse, validation (R1–R7)
crates/aion-package/src/project/discover.rs   gleam.toml/lockfile closure, beam+source discovery (R8–R11)
crates/aion-package/src/project/assemble.rs   package_project, report types (R12–R16)
crates/aion-package/src/project/error.rs      PackagingError
```

`lib.rs` re-exports `package_project`, `PackageOptions`, `ProjectReport`, `PackagedWorkflow`, `ExcludedModule`, `PackagingError`. New deps: `toml` (workspace dep addition).

```rust
/// Options for packaging an already-built Gleam workflow project.
#[derive(Clone, Debug, Default)]
pub struct PackageOptions {
    /// Overrides the single workflow's output path. Error when the project
    /// declares more than one workflow.
    pub output_override: Option<PathBuf>,
}

/// Packages every workflow declared by `<root>/workflow.toml`.
///
/// Pure with respect to the environment: reads only under `root`, writes only
/// the declared outputs, never spawns processes, reads no env vars, never
/// prints. (Meridian links this directly — §8.)
pub fn package_project(
    root: &Path,
    options: &PackageOptions,
) -> Result<ProjectReport, PackagingError>;

pub struct ProjectReport {
    pub packages: Vec<PackagedWorkflow>,   // one per [[workflow]], config order
    pub excluded: Vec<ExcludedModule>,     // R10 filter + D-8 closure exclusions
}

pub struct PackagedWorkflow {
    pub workflow_type: String,             // == entry_module (§1.1)
    pub output_path: PathBuf,              // absolute
    pub package: Package,                  // re-loaded from the written file (R14)
    pub version: WorkflowVersion,
}

pub struct ExcludedModule {
    pub module: String,
    pub package: String,                   // gleam package it came from
    pub reason: ExcludedReason,            // SdkTestOnly | DevDependency
}
```

- **R13** — Pipeline per project: parse+validate config → discover beams/sources once → for each workflow: construct `Manifest` (`version: ManifestVersion::new("unstamped")` placeholder, stamped by the builder; `format_version: CURRENT_FORMAT_VERSION`) → `PackageBuilder::with_source` (source map empty when `include_source = false`) → `write_to_path`.
- **R14 — verify-after-write:** the library re-loads every written archive through `Package::load_from_path` before returning, so the full read-path validation (integrity hash, format version, entry module) gates success and the returned `Package` is the proven artifact, not a hopeful one.
- **R15** — No stdout/stderr/log output from the library; everything a human or agent needs (exclusions, paths, versions) is in `ProjectReport`. No `unwrap`/`expect`/panic paths (workspace lints already enforce).
- **R16** — All relative paths (schemas, outputs) resolve against `root`, never the process cwd.

Error taxonomy — new `PackagingError` (thiserror, `Send + Sync`), wrapping but not polluting `PackageError`:

```rust
#[derive(thiserror::Error, Debug)]
pub enum PackagingError {
    #[error("no workflow.toml found in {root}")]
    ConfigMissing { root: PathBuf },
    #[error("failed to read {path}: {source}")]
    ConfigRead { path: PathBuf, source: std::io::Error },
    #[error("failed to parse {path}: {source}")]
    ConfigParse { path: PathBuf, source: toml::de::Error },   // carries unknown-key detail (R7)
    #[error("invalid workflow.toml: {field}: {reason}")]
    ConfigInvalid { field: String, reason: String },          // R1–R3, R5, R6 semantic checks
    #[error("failed to read schema {path}: {source}")]
    SchemaRead { path: PathBuf, source: std::io::Error },
    #[error("schema {path} is not valid JSON: {source}")]
    SchemaParse { path: PathBuf, source: serde_json::Error },
    #[error("project is not built: {missing} does not exist; run `gleam build` first")]
    ProjectNotBuilt { missing: PathBuf },
    #[error("not a Gleam project: {path} not found")]
    GleamTomlMissing { path: PathBuf },
    #[error("failed to read Gleam metadata {path}: {source}")]
    GleamMetadataRead { path: PathBuf, source: std::io::Error },
    #[error("failed to parse Gleam metadata {path}: {source}")]
    GleamMetadataParse { path: PathBuf, source: toml::de::Error },
    #[error("dependency `{package}` is in gleam.toml but missing from manifest.toml; rebuild")]
    DependencyUnresolved { package: String },                 // D-8 closure integrity
    #[error("failed to read compiled module {path}: {source}")]
    BeamRead { path: PathBuf, source: std::io::Error },
    #[error("compiled module filename is not valid UTF-8: {path}")]
    ModuleNameNotUtf8 { path: PathBuf },
    #[error("module `{module}` is provided by both `{first}` and `{second}`")]
    DuplicateModule { module: String, first: String, second: String },  // R11
    #[error("entry module `{module}` not found in compiled output under {searched}")]
    EntryModuleNotFound { module: String, searched: PathBuf },          // R12
    #[error("failed to read source file {path}: {source}")]
    SourceRead { path: PathBuf, source: std::io::Error },
    #[error("workflows `{first}` and `{second}` both write to {path}")]
    OutputConflict { first: String, second: String, path: PathBuf },    // R6
    #[error("--out is only valid for single-workflow projects ({count} declared)")]
    OutputOverrideAmbiguous { count: usize },
    #[error(transparent)]
    Package(#[from] PackageError),  // ReservedModuleName, write/IO, verify-after-write failures
}
```

---

## 5. CLI UX (`crates/aion-cli`)

```
aion-cli package [PATH] [--out <FILE>] [--build] [--pretty]
```

- **R17** — `PATH` positional, defaults to `.` (argued: operating on the cwd is the universal convention of every project tool the target user already runs — `gleam build`, `cargo build` — not an invented value).
- **R18** — `--out <FILE>`: maps to `PackageOptions::output_override`; library errors if the project declares multiple workflows.
- **R19** — `--build` (D-11): opt-in; runs `gleam build` (inherited stdio → user sees compiler output on stderr) in `PATH` via `std::process::Command` before calling the library. Missing `gleam` binary or non-zero exit → contextual `anyhow` error. Spawning lives only in `aion-cli`.
- **R20** — **`main` restructure:** client construction moves out of the unconditional path (`main.rs:111-117`) into the remote-command branch; `package` must work with no server, no endpoint, no network. Existing remote commands are behaviourally unchanged (their tests pin this).
- **R21** — Output (D-12): success prints one JSON document to stdout via the existing `output::print_json` (respecting global `--pretty`):
  ```json
  {"packages": [{"workflow_type": "hello_world", "output": "/abs/path/hello-world.aion",
                 "version": "<64-hex>", "modules": 23}],
   "excluded": [{"module": "aion_flow_ffi", "package": "aion_flow", "reason": "sdk_test_only"}]}
  ```
  Consistent with every other subcommand (JSON results on stdout, scriptable with `jq`). Nothing else writes to stdout.
- **R22** — Errors render through the existing `anyhow` chain (`Error: failed to package workflow project: invalid workflow.toml: ...`), exit code 1; clap usage errors exit 2 (clap default); success exits 0. Coordinate final rendering with task #52's improvements — `PackagingError`'s message quality (every variant names the offending path/field) is this brief's contribution to that effort.

### DECISION D-11 — does `package` build by default?

- **(A) No-build default; `--build` opt-in.**
- (B) Build by default; `--no-build` opt-out.

**ADOPTED (Tom, 2026-06-11): A, as recommended.**

**Recommendation: A.** Library parity (the no-spawn library is the canonical behaviour; the CLI default should match what Meridian's linked call does), no surprise toolchain invocation from a packaging command, and the failure mode is self-healing: `ProjectNotBuilt` literally tells the user to run `gleam build` or pass `--build`. (B) makes `package`'s default behaviour differ from the library it shells and hides a compiler invocation inside a packaging step.

### DECISION D-12 — CLI output format

- **(A) JSON result document on stdout (house CLI style), `--pretty` supported.**
- (B) Human-prose lines (`wrote hello-world.aion (version abc123…)`).

**ADOPTED (Tom, 2026-06-11): A, as recommended.**

**Recommendation: A.** Every existing `aion-cli` subcommand emits JSON; `package` feeding a deploy script (`jq -r .packages[0].output`) is the primary automation case. Exclusion notices ride in the same document instead of interleaved prose.

---

## 6. Determinism guarantee

- **R23** — The byte-identical reproducibility guarantee survives the new path: for a fixed built tree and fixed `workflow.toml`, `package_project` produces byte-identical `.aion` files and identical content hashes on every run. This holds by construction — discovery feeds `BeamSet` (canonical sort, insertion-order independent: pinned by `beam_set_order_is_independent_of_insertion_order`), sources go through a `BTreeMap`, and `PackageBuilder` already writes deterministic archives (pinned by `identical_inputs_produce_identical_archive_bytes`) — but the new layer must prove it end-to-end, not inherit it on faith:
  - **T-det-1:** package hello-world twice via the library; assert file bytes equal.
  - **T-det-2:** package via the library; construct the same package via direct `PackageBuilder::with_source` calls (manifest hand-built in the test, beams hand-read in deliberately shuffled order); assert byte equality and equal `version_record()`.
  - **T-det-3:** `include_source = false` vs `true` → different bytes, **identical** `ManifestVersion` (hash neutrality at the project level).

---

## 7. Example migration

- **R24** — Delete all seven `examples/*/packager/` directories (binaries, `Cargo.toml`s, lockfiles). No compatibility shims, no zombie code.
- **R25** — Each of the seven examples gains `workflow.toml` + `schemas/input.json` + `schemas/output.json`, transcribing exactly the values in §1.2's variance table (entry module, function, timeout, activities, output filename preserved — `hello-world.aion` etc. via the explicit `output` field where the derived name differs). The committed `*.aion` artifacts are regenerated with the new tool.
- **R26** — Docs: `GETTING-STARTED.md` step 2 and `examples/*/README.md` packaging sections replace `cargo run --manifest-path examples/<x>/packager/Cargo.toml` with `cargo run -p aion-cli -- package examples/<x>` (with the `gleam build` prerequisite or `--build`). New end-user guide `docs/packaging.md`: packaging an arbitrary Gleam workflow — project layout, full `workflow.toml` reference (every field, required/optional/derived), discovery and filtering behaviour, determinism/content-hash story, troubleshooting table keyed by `PackagingError` messages, and a note on the D-10 rename (the server's auto-discovered `aion.toml` is an unrelated server-config file).

---

## 8. Meridian integration contract

What Meridian needs from the library (these are requirements, not aspirations):

- **R27** — Signature stability: `package_project(&Path, &PackageOptions) -> Result<ProjectReport, PackagingError>` is the linked surface; `PackageOptions` is non-exhaustive-friendly (constructed via `Default` + field assignment) so adding options never breaks Meridian's call site.
- **R28** — No side channels: no stdout/stderr writes, no `tracing` emission, no env-var reads, no cwd sensitivity, no process spawning, no panics. Everything observable is the return value (R15/R16 restated as the integration contract — Meridian renders `ProjectReport`/`PackagingError` into agent-tool output itself).
- **R29** — Structured errors: `PackagingError` is `std::error::Error + Send + Sync + 'static` with every variant carrying the offending path/field/module as data (not just formatted text), so Meridian can map variants to actionable agent guidance (e.g. `ProjectNotBuilt` → "run gleam build").
- **R30** — Blocking is acceptable and documented: the function does filesystem I/O synchronously; Meridian wraps it in `spawn_blocking` on its side. No async in the library.

---

## 9. Test plan

**Unit — `crates/aion-package` (in-module tests + tempdir fixtures, no Gleam toolchain needed):**

1. `config.rs`: full-schema round-trip; each required field omitted → precise `ConfigParse`/`ConfigInvalid`; unknown key at top level / `[package]` / `[[workflow]]` → error naming it (R7); `timeout_seconds = 0`; entry module with `$` / `..`; duplicate entry modules; duplicate outputs (explicit and derived collisions); empty `activities` accepted, missing rejected, duplicate/empty strings rejected; zero `[[workflow]]` tables rejected.
2. Schema loading: missing file → `SchemaRead`; invalid JSON → `SchemaParse` with path; valid object and boolean schemas accepted.
3. `discover.rs` against synthetic `build/dev/erlang` trees: prod-closure excludes a dev-dep package (and records it); transitive deps included; `DependencyUnresolved` when the lockfile lacks a `gleam.toml` dependency; missing build dir / missing closure-package ebin → `ProjectNotBuilt` naming the path; `.app`/`fingerprint` skipped; `aion_flow_ffi`+`aion@testing*` filtered only from the `aion_flow` package dir and reported; a root-package module named `aion_flow_ffi` reaching `BeamSet` → `ReservedModuleName` (R10's non-silent guarantee); cross-package duplicate → `DuplicateModule` with both provenances.
4. `assemble.rs`: `EntryModuleNotFound` with searched path; derived vs explicit output naming; `OutputOverrideAmbiguous` for multi-workflow + `--out`; multi-workflow project → N archives, equal content hashes, distinct `deployed_entry_module()`s; `include_source` toggles `src/` entries (all first-party modules, nested paths preserved); verify-after-write happy path returns the re-loaded `Package`.
5. Determinism: T-det-1/2/3 (§6) over a synthetic project.

**Integration — real hello-world (`crates/aion-package/tests/package_project_hello_world.rs`):** requires `examples/hello-world/build/dev/erlang` to exist; **runtime-gated** per house rules (check at runtime, emit a skip line, return `Ok(())` — never `#[ignore]`). Package via the library; byte-compare against a direct `PackageBuilder` construction reproducing the migrated `workflow.toml` values; `Package::load_from_path` the output and assert `version_record()` fields (entry module, activities, schemas) match the config.

**Engine e2e (Wave 2):** boot the library-built `hello-world.aion` through the engine load path (pattern from existing `crates/aion` package-loading tests) and assert the workflow type registers under its deployed name. *Engine e2e may slip to whichever Wave-2 window has the cargo build farm free — it gates the wave's completion, not Wave 1's.*

**CLI — `crates/aion-cli`:** clap parse tests in the existing style (`package` with/without `PATH`, `--out`, `--build`); a test pinning that `package` never constructs a client (R20 — e.g. dispatch-shape test or a refused-endpoint construction proof); runtime-gated end-to-end run against hello-world asserting the R21 JSON shape and exit 0; error path (no `workflow.toml`) asserting exit 1 and an `Error:` chain naming the root.

**Gates (every wave):** `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, full `cargo test -p aion-package -p aion-cli`; no `#[allow]`/`#[ignore]`/`_var` bypasses.

---

## 10. Wave plan

- **Wave 1 — library (1 agent; `crates/aion-package` only):** `project/` module set (config, discover, assemble, error), `toml` workspace dep, `lib.rs` re-exports, unit tests 1–5 + gated hello-world integration test. *Exit: gates green; `package_project` packages hello-world byte-identically to a direct builder construction.*
- **Wave 2 — CLI + migration + docs (1 agent; `crates/aion-cli`, `examples/`, docs):** `package` subcommand + `main` client restructure (rebase on / coordinate with task #52); delete seven packagers; seven `workflow.toml` + schema files; regenerate committed `.aion` artifacts; GETTING-STARTED + example READMEs + new `docs/packaging.md`; CLI tests; engine-boot e2e. *Exit: a fresh checkout follows GETTING-STARTED end-to-end with no packager crates in the tree.*
- **Wave 3 — review:** rigorous Fable-model review per CLAUDE.md (brief + intent + files, reviewer explores beyond), determinism tests re-run, full workspace gates. Nothing deferred.

Waves 1 and 2 are sequential (the CLI consumes the library's final API). Estimated size: W1 ≈ 1.2–1.5k LoC incl. tests; W2 ≈ 700 + doc prose.

---

## 11. Out of scope (explicit)

- **Schema inference from Gleam types** — deriving `input_schema`/`output_schema` from the entry function's types needs SDK-side type introspection or codegen annotations; future brief. `workflow.toml` paths are the v1 contract.
- **Server-side authoring validation loop** (upload-and-validate against a running server, dry-run deploys).
- **Per-activity timeouts/retry policy in the manifest** — format change, `format_version` bump, own brief (D-4).
- **User-configurable module excludes** (D-9) and **packaging non-Gleam BEAM projects** — no consumer; revisit on concrete need.
- **`gleam export erlang-shipment` support, package signing, registry publishing.**

---

## Decision summary — all adopted (Tom, 2026-06-11)

| # | Question | Resolution |
|---|---|---|
| D-1 | Schema representation | **ADOPTED A** — paths to JSON files, required |
| D-2 | Timeout field | **ADOPTED A** — `timeout_seconds` integer ≥ 1, required |
| D-3 | `entry_function` default | **ADOPTED A** — required, no `"run"` default |
| D-4 | Activities shape | **ADOPTED A** — flat string list (manifest carries only `activity_type`) |
| D-5 | Source inclusion | **ADOPTED B** — default `true`, all first-party `src/**`, opt-out; hash-neutral |
| D-6 | Multi-workflow | **ADOPTED A** — `[[workflow]]` array from day one; one `.aion` per entry |
| D-7 | Output naming | **ADOPTED B** — optional `output`; derived `<entry_module>.aion` otherwise |
| D-8 | Build dir + dep filtering | **ADOPTED B** — `build/dev/erlang` + prod-dependency-closure filter from gleam.toml/manifest.toml |
| D-9 | Test-module filter | **ADOPTED A** — hardcoded SDK list only, scoped to `aion_flow`'s ebin; reported, never silent for user modules |
| D-10 | `aion.toml` name collision with server auto-discovery | **ADOPTED B — RESOLVED by rename**: packaging descriptor is `workflow.toml`; server's `aion.toml` keeps its name; `deny_unknown_fields` retained as defence in depth |
| D-11 | CLI build behaviour | **ADOPTED A** — no-build default, `--build` opt-in |
| D-12 | CLI output | **ADOPTED A** — JSON document on stdout, house style |
