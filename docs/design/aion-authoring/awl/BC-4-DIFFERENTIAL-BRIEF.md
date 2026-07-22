# BC-4 build brief — the differential harness

Lane opened 2026-07-22 on the operator's dispatch. Roles per the ratified plan
(`AWL-BC-BUILD-PLAN.md` row BC-4, decisions 7/11/13/16): Opus builds the
harness, the Fable seat adjudicates every divergence. Branch:
`dev/bc4-differential`. Board task: #43.

## What BC-4 proves

Every fixture **both backends accept** runs through a **real engine** and
produces **byte-identical durable event trails** after normalizing exactly
five field families. Plus: the ABI contract table asserted as tests, and the
rev-2 adversarial corpus authored and included.

The two backends:

- **Reference**: `.awl` → `aion_awl::gleam::emit` (Gleam source) → `gleam
  build` → `aion_package::package_project` → `Package`.
- **Direct**: `.awl` → `aion_awl::parse` → `aion_awl::mir::lower` →
  `aion_awl::mir::select` → `.beam` bytes → **spliced as the entry module
  into the reference package's SDK closure** via
  `PackageBuilder::new(ref_pkg.manifest().clone(), BeamSet::new(modules)?)`
  (the capstone Deliverable-B pattern).

Both packages run through `EngineBuilder` (in-memory store + visibility,
`scheduler_threads(1)`, one shared deterministic `ActivityDispatcher`
implementation) → `start_workflow` → `result` → `store.read_history` →
`shutdown`. Compare `trail_norm::normalized_trail(&a)? ==
normalized_trail(&b)?`.

## Deliverables

1. **Port the seed normalizer.** Copy
   `crates/aion/tests/common/trail_norm.rs` VERBATIM from branch
   `awl-bc1-capstone` (`git show awl-bc1-capstone:crates/aion/tests/common/trail_norm.rs`).
   It already normalizes exactly the five ratified families (`recorded_at`,
   `fire_at`, `workflow_id`, `run_id`/`parent_run_id` first-appearance
   placeholders, `package_version`). Before relying on it, assert (as a unit
   test) that `aion_core::Event`'s serde field names on main still match those
   six literals. **Decision 11 is law: any OTHER field that differs between
   backends is a divergence to adjudicate — NEVER extend the normalizer to
   make a red case green.**

2. **The differential harness** over the covered ratchet
   (`crates/aion-awl/src/mir/covered.rs`, currently 70 entries). New
   integration test root `crates/aion/tests/awl_bc4_differential.rs` fanning
   into `awl_bc4_differential/` submodules via `#[path]` (the
   `runtime_codecs.rs` idiom), every file ≤500 code lines. For each covered
   fixture: build both packages, run both, compare normalized trails.
   - Fixtures whose steps dispatch activities get canned deterministic
     results from one dispatcher implementation used for BOTH runs (echo of
     activity type + stable payload), so trails can only diverge on backend
     behavior.
   - A fixture that `lower` refuses (`LowerError::Unsupported`/`Message`) is
     out-of-intersection: record it in the run report (see 5), skip, never
     fail. `lower` re-runs the checker internally; no separate check call.
   - Amortize `gleam build`: batch reference modules into as few throwaway
     projects as feasible (`runtime_codecs/harness.rs::gleam_build` pattern);
     scratch under `target/awl-test-scratch/`, NEVER `/tmp`. Missing `gleam`
     on PATH is a hard test failure, not a skip (example_build.rs law).

3. **ABI contract tests.** Assert the `AWL-BC-IR.md` §7 IR-table rows
   (lines ~695-736 — the authoritative, capstone-grounded contract; draft §4
   is the summary): IR-13 entry ABI (`run/1` → `{ok, ResultBinary}`, error →
   `{error, AwlErrorTerm}`), IR-5/6/7/8 value representations, IR-10 SDK
   constructor atoms/arities, IR-12 module-name mangling, IR-15 exact export set
   (`definition/0`, `run/1`, `execute/1`, no `module_info`), IR-2 float literal
   byte-parity, writer-contract chunk set/order + header counts. Where a row is
   already pinned by an existing aion-awl test, reference it rather than
   duplicating; add what's missing at the package / loaded-module level.

   **IR-14 (calling convention) — coordinator's ruling (round 3):** x/y register
   allocation and tail-call instruction shape are NOT provable at the durable
   trail level, and BC-4 does not assert them. They are exercised structurally by
   aion-awl's `select` tests (which build and validate the instruction stream),
   and direct byte/instruction-level assertion is DEFERRED to BC-5 codegen
   inspection. BC-4 therefore makes no IR-14 claim; the ABI tests and their
   comments state only what they prove.

4. **The eight adversarial fixtures** (net-new; none exist): empty fork
   collection, runtime-sized fork, nested handlers, timeout-inside-retry,
   unicode, max-arity records, zero-step workflow, child spawn. Author in the
   rev-2 surface under `crates/aion-awl/tests/fixtures/rev2/` (study
   `MANIFEST.md` + nearest analogues: `dag-fork/valid/child_collection_fork`,
   `fork_collection_join`, `loop_counting_until_max`,
   `header-types/valid/zero_inputs`, `declarations/valid/child_call_awaited`).
   Each that lowers joins the `covered.rs` ratchet AND the differential
   corpus; each that refuses gets a span-anchored entry in the
   deferred-refusal ratchet (`mir/deferred_tests.rs` idiom) and is recorded
   out-of-intersection. Do not force coverage: an honest refusal is a valid
   outcome (decision 16).

5. **The divergence report.** The harness accumulates every divergence
   (fixture, event index, JSON pointer, both values) and every
   out-of-intersection refusal (fixture, refusal text) and asserts on
   divergences with the full report in the failure message. Refusals are
   reported via a pinned expected-list (ratchet style), so intersection
   shrinkage is loud.

## Laws (workspace, enforced by clippy -D warnings)

No `unwrap`/`expect`/`panic`/`todo` — tests included: `type TestResult =
Result<(), Box<dyn std::error::Error>>` and `?` everywhere. No
`#[allow]`/`#[expect]`/`#[ignore]`/`_var` suppressions; runtime gates read an
env var and `return Ok(())`. Files ≤500 code lines; `mod.rs` re-exports only.
Doc comments on every helper, identifiers backticked. `cargo fmt --all`
before every commit.

**Cargo.toml: change NOTHING.** Main already pins `beamr = { version =
"0.15.4", features = ["json", "encode"] }`. The capstone branch's
`[patch.crates-io]` beamr path-dep is D-BC4 poison and never ports. The
`capstone_twin` fixture does not port either (the corpus is the rev2 tree).

## Gates (run in this worktree, redirect output to files, echo exit codes)

1. `cargo fmt --all`
2. `cargo clippy --workspace --all-targets` → exit 0
3. `cargo test -p aion-rs` (crate name is `aion-rs`, dir `crates/aion`) → exit 0
4. `cargo test -p aion-awl` (fixture/ratchet changes) → exit 0

Commit on `dev/bc4-differential` in logical units with trailer
`Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`. **Never push, never
merge** — landing is the coordinator's hands after review and the tear.

## Adjudication protocol

Any normalized-trail divergence, any sixth field family, any ABI row that
does not hold: STOP work on that item, record it fully in the divergence
report, and surface it in your final summary for the Fable seat to
adjudicate. Divergences are the product BC-4 exists to find — never paper
over one.
