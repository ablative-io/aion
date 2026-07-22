# BC-5 build brief — CLI beam target + codegen inspection (IR-14)

Lane opened 2026-07-22 on the operator's direct go ("make sure it's all going
to work with the ops console and the authoring view"). Roles as BC-4: Opus
builds, Norn reviews adversarially, the Fable seat's read of the final bytes
is the gate of record. Branch: `dev/bc5-codegen`.

## Ground truth (verified at the bytes 2026-07-22 — build on this, not on
older docs)

The direct bytecode backend is ALREADY the production deploy path for the ops
console: `POST /awl/deploy` → `aion_awl_package::compile_and_assemble_awl`
(`crates/aion-awl-package/src/prepare.rs:40`) → `aion_awl::compile(source,
schema_root)` (`crates/aion-awl/src/compile.rs:147`, the `lower → verify →
select → sidecar` pipeline) → `.aion` archive → engine hot-load
(`crates/aion-server/src/awl/run_loop.rs:152-224`). The console
(`apps/aion-ops-console/src/features/authoring/lib/guided-facade.ts:31`)
never calls `/awl/emit` and never touches Gleam.

What does NOT exist: a CLI beam target. `aion awl emit`
(`crates/aion-cli/src/awl.rs:60-66`, `emit_command` at :127) emits only Gleam
text via `aion_awl::emit_artifact_in`, plus a Gleam-project-shaped sidecar
(`write_entry_sidecar`, awl.rs:218-236). And IR-14 (calling convention) is
formally NOT asserted anywhere at the instruction level
(`AWL-BC-IR.md:715`, deferred to this lane by the BC-4 round-3 ruling).

## Deliverable A — `aion awl emit --target beam`

1. Add `--target <gleam|beam>` to `AwlCommand::Emit` (clap `value_enum`,
   default `gleam`). The gleam path's behaviour and output must stay
   byte-identical to today — prove it with an existing-output regression
   test, not by inspection.
2. The beam path calls **the same seam the server uses**:
   `aion_awl::compile(source, schema_root)` with `schema_root` = the
   document's parent directory (parity with the server's staged-imports
   root semantics; divergent `schema("...")` resolution between CLI and
   console is a defect, not a nuance).
3. Output: every compiled module is written — the entry module AND every
   synthesized child module; silently dropping one is a hard failure. Binary
   never goes to stdout: `--target beam` requires `--output`; refuse with a
   clear typed error otherwise. Propose the concrete file layout (single
   `.beam` file vs. directory of `<module>.beam`) in your first commit
   message; the coordinator ratifies it at review. Document the layout in
   the clap help text.
4. Sidecar: beam-shaped, derived from `CompiledWorkflow`
   (`workflow_name`, `input_schema`, `output_schema`, `actions`,
   `synthesized_workflows`, `timeout`) — NEVER the Gleam
   `project_metadata` shape next to `.beam` bytes.
5. **The ops-console compatibility proof (the operator's condition):** a test
   asserting the CLI-emitted beam bytes are byte-identical to the
   corresponding module bytes inside `compile_and_assemble_awl`'s archive
   for the same source. One seam, zero drift — this is the guarantee that
   CLI output and console-deployed output can never diverge.
6. Out of scope, do not touch: the `/awl/emit` HTTP contract (the console
   never calls it), the legacy `/authoring/compile` Gleam path,
   `aion package --build` / `generate` (Gleam-project tooling, unrelated),
   and the frontend `DiagnosticClass` taxonomy (beam-path refusals surface
   through the existing `error` class; note the stale `emit_subset`
   acceptance at `facade.ts:25,262` in your report — a future-note, not
   this lane's work).

## Deliverable B — codegen inspection + the IR-14 assertion

1. An instruction-level inspection harness (new test module in
   `crates/aion-awl`, mirroring the `select` test idiom) that decodes
   `select()` output with the beamr 0.15.4 decoder (the BC-4 `abi.rs`
   techniques: `parse_beam_chunks` declared-length discipline; remember the
   Code decoder breaks-and-returns-Ok at `int_code_end` — reuse the BC-4
   truncation-witness pattern wherever "fully decoded" matters).
2. Assert the IR-14 row (`AWL-BC-IR.md:715`) plus its §11 refinements, over
   the full covered ratchet AND targeted per-shape fixtures (at least: one
   framed function, one frameless body, one loop for tail-call shape):
   - args arrive in `x0..x(n-1)`, result returned in `x0`;
   - framed (tier-2) functions bracketed `allocate`/`deallocate` per R8's
     conservative predicate (`frame_size > 0`); frameless bodies use no Y;
   - `trim` never emitted (R6);
   - single shared exit, linearly last (R7);
   - Y touched only by `move`; `Live` on `TestHeap`/`GcBif` equals the
     per-burst X high-water (R8);
   - routes/loop recursion emitted as tail calls.
   Where a §11 claim proves false against the bytes, that is a divergence to
   adjudicate — STOP on that item and surface it; never weaken the assertion
   to match the emitter.
3. On green, amend `AWL-BC-IR.md`: the IR-14 row's "NOT asserted" annotation
   becomes a reference to the inspection tests (name them). The BC-4 brief
   is a historical record — do not edit it.
4. D1 from §11.7 (missing `live_after` annotations on the five fused
   call-bearing ops) may block precise R8 assertions; if so, extending the
   annotation is in scope as an additive MIR increment — but recomputed
   liveness stays authoritative, per §11.2.

## Laws (workspace, enforced by clippy -D warnings)

No `unwrap`/`expect`/`panic`/`todo` — tests included: `type TestResult =
Result<(), Box<dyn std::error::Error>>` and `?` everywhere. No
`#[allow]`/`#[expect]`/`#[ignore]`/`_var` suppressions. Files ≤500 code
lines; `mod.rs` re-exports only. Doc comments on every helper, identifiers
backticked. `cargo fmt --all` before every commit (never a format check).

**Cargo.toml: change NOTHING.** beamr stays `0.15.4` (features
`["json", "encode"]`). If the decoder lacks something the inspection needs,
STOP and report — beamr changes route through Artemis Peach's seat, never a
local pin bump or patch. **Target directories are defaults only — never set
`CARGO_TARGET_DIR`** (operator's standing order 2026-07-22). Scratch lives
under `target/awl-test-scratch/`, never `/tmp`. Never pipe cargo/gleam
output through grep/tail/head — redirect to a file and echo the exit code.

## Gates (run in this worktree, redirect output to files, echo exit codes)

1. `cargo fmt --all`
2. `cargo clippy --workspace --all-targets` → exit 0
3. `cargo test -p aion-awl` → exit 0
4. `cargo test -p aion-cli` → exit 0
5. `cargo test -p aion-rs` (dir `crates/aion`) → exit 0
6. Live CLI proof: build the CLI binary, run `aion awl emit --target beam`
   against `examples/awl_hello/awl_hello.awl` (or the committed hello
   fixture), and byte-compare the output against the archive-internal module
   bytes. Record the exact commands and exit codes in your final summary.

Commit on `dev/bc5-codegen` in logical units with trailer
`Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`. **Never push,
never merge** — landing is the coordinator's hands after the Norn rounds,
the Fable read, and the tear.

## Adjudication protocol

Any §11/IR-14 claim that does not hold against the decoded bytes, any drift
between CLI and archive module bytes, any gleam-path behaviour change: STOP
work on that item, record it fully, and surface it in your final summary for
the Fable seat to adjudicate. Divergences are the product this lane exists
to find — never paper over one.
