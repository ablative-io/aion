# AWL-BC — build plan of record

Status: ACTIVE. Started 2026-07-11 on Tom's go (Lane A). Tracker: the #240 line
of work. Design of record: `AWL-BC-DESIGN-DRAFT.md` (ratified 2026-07-09) — this
plan re-points that design at the **rev-2 surface** (`AWL-2-SPEC.md`, front end
landed `5079eaaa`, rulings `1fab6e1d`) and folds in the 2026-07-11 recon
findings. Where this document and the draft disagree, this document wins.

The mission is unchanged: **`.awl → .beam` directly from aion's own Rust.** No
Gleam compiler, no erlc, no OTP on the authoring machine. Check-clean means it
runs; every diagnostic is span-anchored to the author's source; the same `.awl`
produces the same bytes (#218 dissolves). The Gleam emitter survives as the
reference implementation and differential oracle.

## 1. Recon deltas vs the ratified draft

Three-agent recon (rev-2 emitter surface / beamr loader / package-execution
path, 2026-07-11) confirmed the architecture and corrected these specifics:

1. **No `LocT` chunk.** The loader never reads it. The writer's chunk set is
   `AtU8, Code, ImpT, ExpT, FunT, LitT, StrT, Line` — only `AtU8` and `Code`
   are strictly required at load. Chunk order is not enforced on read; the
   writer picks one canonical order.
2. **An ETF encoder already exists** (`beamr::etf::encode`, ungated) plus a
   shared tag table (`etf/tags.rs`). The `LitT` literal encoder operates on
   `loader::decode::chunks::Literal` rather than runtime `Term`, but the tag
   encodings are cribbable. `jit/aot_format.rs` is in-repo precedent for a
   hand-rolled binary container writer.
3. **The reusable types are already `pub`**: `Operand` (incl.
   `TypedRegister`), `Instruction` (75 variants incl. a `Generic` catch-all
   that makes a total re-encoder possible), `ImportEntry`/`ExportEntry`/
   `LambdaEntry`/`LineInfo`/`Literal`, `ParsedModule`. The existing
   `instruction_opcode` reverse map covers only ~20 variants — the writer
   needs its own complete table. 127 opcodes supported; `is_tagged_tuple`
   (159) is fully supported in interpreter and JIT.
4. **Five validation layers** an emitted module must survive: IFF structural
   checks; `DecodeBudget`; Code-header consistency (`sub_size=16`,
   `instruction_set=0`, `label ≤ label_count`, `opcode ≤ opcode_max`);
   capability policy on native imports; `validate_module` (export labels,
   register bounds, frame-size tracking via Allocate/Deallocate/Trim,
   intra/inter-module call-arity checks).
5. **~45% of a rev-2 generated module is boilerplate** (measured on
   `awl_hello.gleam`, 344 lines): a fully-fixed block (`AwlError` + its
   codec/encoder/decoder, 15 builtin leaf codec fns, `try` + 5 error mappers,
   `awl_index`, plus flag-gated `awl_raw_codec`/`awl_decoded`/
   `json_value_codec`) ≈ 34%, and name-substituted shells (`run()`,
   `definition()`, imports) ≈ 11%. The bulk of the remaining "specific" glue
   is the per-type codec trios (~105 lines in awl_hello) — structurally
   uniform templates that are a pure function of the emitter's `TypeEnv`.
6. **Type information dies at the Gleam boundary today.** The emitter holds a
   full `TypeEnv` (JSON-Schema provenance, optionality, outcome directions,
   binding→type liveness); the generated Gleam erases it and Gleam re-infers.
   **No `.gleam_types` sidecar exists anywhere in aion.** On the beamr side
   the sidecar format is documented (`gleam-types` crate: magic
   `GLEAM_TYPES\0` v1, per-function `TypeDescriptor` params/returns), consumed
   ONLY by the JIT/AOT path (`jit/aot.rs` reads
   `beam_path.with_extension("gleam_types")`); the interpreter decodes and
   discards in-bytecode `TypedRegister.type_index`.
7. **The real runtime import boundary is `aion_flow_ffi`** — the SDK's durable
   primitives bottom out in engine-registered NIFs under that reserved
   namespace. Generated code never calls it directly; it calls the `aion_flow`
   Gleam modules. The complete SDK call surface of generated code is
   enumerated in the recon (aion/workflow: define, run, all, map, spawn,
   spawn_and_wait, receive, with_timeout, sleep; aion/activity: new,
   task_queue, retry, timeout, node; aion/codec, aion/signal.new,
   aion/duration.milliseconds, aion/error.terminal; gleam stdlib:
   dynamic/decode, json, list, option, int/float/string/bool.compare).
8. **Combinators need no new VM ops.** `filter/map/sort/count` lower to
   `gleam/list` calls with comparator/accessor closures today; in bytecode
   they are `call_ext` + `make_fun` shapes like everything else. The
   "combinator ops" concern from the roadmap is closed by construction.
9. **Everything from `aion_package::package_project` down is reused
   unchanged**: `BeamSet` (sorted canonical order, reserved-name rejection),
   the framed SHA-256 content hash, the deterministic ZIP builder, deploy →
   `catalog.load_package` → beamr `prepare_module` → **content-hash rename
   machinery** (`runtime/module.rs` rewrites module names and every
   cross-module reference to `<logical>$<hash>`, rematerializes the constant
   pool, recomputes lambda unique_ids). Emitted modules must be compatible
   with that rewriting — same structures erlc output has, nothing exotic.
10. **#218 concretely**: the packaging layer is already deterministic; the
    nondeterminism lives upstream in gleam/erlc output bytes. BC dissolves it.
11. **The differential harness is net-new.** The trail oracle taps
    `EventStore::read_history`; no run-id/timestamp normalization tooling
    exists anywhere in the test tree. `replay_inspect.rs` is single-backend
    replay determinism, not a two-backend oracle.
12. **The Gleam emitter refuses/degrades some legal rev-2 shapes** with
    `// awl stopgap:` comments (route-targeted `after`-dependent steps,
    two-entry regions; richer parallel bodies degrade to written sequential
    order). `graph.rs`/`steps.rs` ticket those to #240. See D-BC3.

## 2. Locked decisions

- **D-BC1 — rev-2 is the only source surface.** `lower` consumes the rev-2
  checker's `CheckedDocument`; the MIR mirrors the rev-2 emitter's lowering
  decisions (regions, Kahn layers, liveness-threaded params, tail-call
  routing, loop functions with language-owned counters, outcome case trees).
  No AWL-0 compatibility anywhere.
- **D-BC2 — descriptor-driven codecs in BC-0.** The per-type codec trios
  leave the emitted surface: the SDK gains a generic codec engine (Gleam,
  `aion_flow`) that walks a type-descriptor value, and the emitted module
  carries type descriptors as literals instead of per-field encode/decode
  code. This is the single biggest reduction of the BC-3 instruction-selection
  surface, and it makes D4 optionality (absent-vs-null) ONE canonical
  implementation instead of a re-derivation per module. Behavior must be
  observably identical: goldens re-baselined deliberately, existing e2e
  trails unchanged, compile proofs green. **Fallback (pre-authorized):** if
  the generic engine fights Gleam's type system beyond reasonable effort,
  BC-0 ships hoist-only (fixed glue) and the codec trios become ~4 more MIR
  template shapes in BC-2/BC-3. The fallback costs BC-3 size, not
  correctness.
- **D-BC3 — parity first, capability second.** BC-2..BC-5 target exact
  behavioral parity with the Gleam emitter, including its stopgap refusals —
  the differential oracle only means something over the intersection both
  backends accept. Closing the stopgap gaps (route-targeted after-steps,
  two-entry regions, true parallel step bodies) is **BC-6**, a follow-up
  after the oracle soaks, gated on single-backend replay-determinism +
  semantic tests since the oracle cannot cover shapes the reference refuses.
- **D-AOT1 — no type erasure: emit `.gleam_types` sidecars.** BC emits the
  sidecar bytes for every generated module directly from the `TypeEnv`
  (via the `gleam-types` crate's serialize; signatures restricted to the
  descriptor set the JIT actually specializes). Deterministic bytes, golden
  tested. Shipping them in the `.aion` archive and plumbing
  `load_companion_into_cache` through the catalog is a BC-5 design item
  (additive archive entry if tolerated, else format_version 2); wiring the
  server JIT to consume them at runtime is post-BC follow-up, NOT a BC gate.
- **D-AOT2 — the IR is a documented contract.** BC-2 produces
  `AWL-BC-IR.md`: the MIR node set, the canonical-model→MIR mapping, the
  complete import table (= the enumerated runtime-capability set — the
  tree-shake manifest the beamr AOT track needs), and the ABI representation
  table from the draft §4. Kept current as a BC-2..BC-5 acceptance item.
  MIR stays private to `aion-awl` (ratified decision 3 stands).
- **D-BC4 — beamr dependency policy** (ratified decision 4 stands): the
  `encode` module lands in beamr behind a new `encode` cargo feature
  (default-off, zero new deps — flate2 is already a hard dep); aion path-deps
  beamr during development **in worktrees only** — a `Cargo.toml` pointing at
  a path never merges to aion main; beamr releases 0.14.0 once, at BC-5,
  and aion main moves to it then.
- **D-BC5 — the capstone is an evidence gate, not a merge.** BC-1's capstone
  (hand-built minimal module → loads → validates → completes a real engine
  run with a trail matching its Gleam-built twin) runs in a path-dep aion
  worktree and is verified by the operator's hands. The beamr `encode` module
  + round-trip suite merge to beamr main at BC-1; the capstone harness
  carries forward unmerged and becomes the seed of BC-4's differential
  harness. BC-2/BC-3 do not start until the capstone is green (ratified).
- **D-BC6 — SDK artifact bundling is a BC-5 design item.** Without gleam on
  the author machine, `aion package` must supply the prebuilt `aion_flow` +
  gleam stdlib `.beam` closure itself (embedded in the aion binary or
  shipped as versioned data). Design decided and implemented in BC-5;
  version-stamped per the draft's ABI-drift mitigation.

## 3. Phases

| Phase | What | Oracle | Model balance |
|-------|------|--------|---------------|
| **BC-0** | `aion_flow` hoist + descriptor-driven codec engine (D-BC2); re-point the Gleam emitter at the SDK helpers; re-baseline goldens; regenerate committed example modules | full aion-awl/aion-cli suites; compile proofs; awl-hello e2e trail unchanged; generated-module line count drops ≥40% *(descriptor-full figure — hoist-only shipped 37.8%, ruling in decision 10)* | Opus implements, Fable panel + design of the descriptor value shape |
| **BC-1** | beamr `loader/encode/` (feature `encode`): IFF writer, 8 chunk encoders, compact-operand encoder (incl. multi-byte/bignum packing), complete Instruction→opcode table, LitT ETF literal encoder; then the **capstone** | round-trip `decode(encode(decode(x))) == decode(x)` over the full corpus (~79 beamr fixtures + aion ebin trees); re-encoded modules pass `validate_module`; capstone per D-BC5 | Opus implements the mirror (round-trip suite is the ratchet), Fable reviews the API + builds and proves the capstone |
| **BC-2** | MIR + `lower` from rev-2 `CheckedDocument`; `AWL-BC-IR.md` (D-AOT2); `.gleam_types` emission from TypeEnv (D-AOT1); *includes the per-type codec-trio template shapes inherited from the hoist-only fallback (decision 9)* | MIR golden files per fixture; sidecar goldens; IR doc review | Fable designs the MIR (competing designs + judge panel), Opus implements lower against the ratified design |
| **BC-3** | `select` + register allocation → `Instruction` sequences; assemble via beamr encode | emitted modules load + validate through all five layers; per-shape unit tests; every checking fixture emits | Fable on regalloc/select design + review, Opus on per-shape mechanical coverage |
| **BC-4** | Differential harness (net-new, seeded by the capstone's `trail_norm.rs`): every fixture both backends accept, through a real engine, identical durable event trails after normalizing exactly five field families — `recorded_at`, timer `fire_at`, `workflow_id`, `run_id`/`parent_run_id` (first-appearance-ordered placeholders), `package_version` (decision 11; any further field is a divergence to adjudicate, never a silent addition); ABI contract tests (draft §4 table + the observed entry ABI: `run/1` → `{ok, ResultBinary}`); adversarial corpus re-expressed in rev-2 (empty fork collection, runtime-sized fork, nested handlers, timeout-inside-retry, unicode, max-arity records, zero-step workflow, child spawn) | the normalized trails, byte-for-byte | Opus builds harness, Fable adjudicates any divergence |
| **BC-5** | Wire-in: `aion awl emit --target beam`; `aion package` native for `.awl` projects (D-BC6 bundling); sidecars into the archive (D-AOT1); deterministic-bytes test (#218: package twice → identical content hash); beamr 0.14.0 release; aion main to crates.io beamr | e2e: `.awl → package → deploy → run` on a real server with zero Gleam toolchain; #218 test; existing Gleam-path packaging untouched | mixed; operator hands on the release |
| *BC-6 (follow-up, not in this build)* | close the stopgap refusals (D-BC3) | replay determinism + semantic tests | — |

Sequencing: **BC-0 ∥ BC-1** (different repos), then BC-2 → BC-3 → BC-4 → BC-5
strictly behind the capstone. If the capstone surprises (validator rejection,
ABI mismatch, missing opcode), this plan and the draft get amended before
BC-2 moves — the round-trip writer is needed under any outcome, so nothing is
wasted.

## 4. Method

Same discipline that built the rev-2 front end:

- **Ultracode workflows in sequence, one per phase-group** (workflow 1 =
  BC-0 ∥ BC-1; workflow 2 = BC-2 + BC-3; workflow 3 = BC-4 + BC-5), the
  operator in the loop between them. Workflows NEVER merge or push.
- **Worktrees**: each lane in its own git worktree
  (`<repo>/.yggdrasil-worktrees/<branch>`); aion path-deps on the beamr
  worktree stay in worktrees only (D-BC4).
- **Panels**: every implementation phase ends with a 3-lens adversarial panel
  (spec/design-fidelity, correctness, conventions) + bounded fix round +
  independent verify. Conventions bar: files ≤500 code lines, mod.rs
  re-exports only, no unwrap/expect outside tests, no bypass attributes.
- **Ratchets instead of known_red**: this is an additive build, not a
  rebuild. The permanent ratchets are (1) the round-trip corpus, (2)
  `validate_module`, (3) MIR/sidecar goldens, (4) the differential trails.
  Each phase adds its ratchet and every later phase keeps it green.
- **Operator hands** (non-negotiable): all gates re-run bare at merge review
  (never through grep/tail pipes), merges `--no-ff`, pushes, and the BC-1
  capstone + BC-4 divergence verdicts reviewed personally. Model balance per
  Tom (2026-07-11): Opus on recon/mechanical/round-trip mirror work, Fable on
  MIR design, instruction-selection review, capstone verification.

## 5. Risks (updated from draft §9)

| Risk | Mitigation |
|------|------------|
| Compact-operand encoder subtly wrong (multi-byte/bignum packing) | the round-trip corpus is exactly the test for this; hostile-input budget limits don't apply to writing, but header counts do — encoded header counts derived from the instruction stream, never hand-set |
| Descriptor-driven codec engine (D-BC2) changes observable JSON behavior | goldens re-baselined deliberately + awl-hello e2e trail unchanged + compile proofs; fallback pre-authorized |
| Content-hash rename machinery mangles emitted modules | round-trip modules through `register_module_with_renames` in BC-1 capstone; emit only structures erlc output uses |
| ABI drift on Gleam/SDK version bump | draft §4 contract tests land in BC-4 and run forever |
| Two-repo coordination (beamr path-dep leaking to aion main) | D-BC4: path-deps in worktrees only; CI would fail on a leaked path-dep anyway (no such path on runners) |
| Differential harness flakes (load-sensitive e2e family #243/#244/#248) | harness runs serially per fixture; provenance protocol at merge review |
| Scope creep toward general Gleam compiler | the §5 shape inventory stays closed; anything not on it goes into `aion_flow` as SDK code (BC-0 direction) |

## 6. Decision log

| # | When | What | Why |
|---|------|------|-----|
| 1 | 2026-07-11 | Plan authored; draft re-pointed at rev-2 (D-BC1) | front end landed 5079eaaa; AWL-0 grammar dead |
| 2 | 2026-07-11 | D-BC2 descriptor-driven codecs, with pre-authorized fallback | ~105 lines/module of per-field code becomes a literal; one canonical D4 implementation; biggest BC-3 shrink |
| 3 | 2026-07-11 | D-BC3 parity-first; stopgap closure deferred to BC-6 | oracle integrity over capability; the reference can't oracle shapes it refuses |
| 4 | 2026-07-11 | D-AOT1/D-AOT2 locked (sidecars + IR contract doc) | Tom's AOT cooperative-planning ask (2026-07-11): typed bytecode no erasure; enumerated capability set = tree-shake manifest |
| 5 | 2026-07-11 | LocT dropped from writer chunk set | loader never reads it (recon correction) |
| 6 | 2026-07-11 | Combinators = ordinary call_ext shapes, no VM ops | they already lower to gleam/list calls (recon) |
| 7 | 2026-07-11 | D-BC5 capstone = evidence gate, harness unmerged until BC-4 | keeps aion main free of path-deps while honoring the ratified capstone-before-BC-2 rule |
| 8 | 2026-07-11 | D-BC6 SDK artifact bundling deferred to BC-5 design | not needed before wire-in; version-stamping folds into ABI-drift mitigation |
| 9 | 2026-07-11 | BC-0 shipped HOIST-ONLY via the design's §9(iii) pre-authorized fallback (descriptor-full contested in two adversarial review rounds); descriptor-full retained in AWL-BC-CODEC-DESIGN.md as the worked reference | per D-BC2; the per-type codec trios stay generated and become ~4 MIR template shapes in BC-2/BC-3 |
| 10 | 2026-07-11 | Operator ruling: 37.8% generated-module shrink (awl_hello 344→214) ACCEPTED for hoist-only — the plan's ≥40% was descriptor-full's measurement | structural to the fallback, not a defect; behavior oracle closed separately (decision 13) |
| 11 | 2026-07-11 | BC-4 trail normalization = exactly five field families: recorded_at, fire_at, workflow_id, run_id/parent_run_id, package_version | capstone evidence: package_version can never match raw between byte-different productions; ids derived from sequence positions compare raw |
| 12 | 2026-07-11 | Writer emits no `int_code_end` terminator and skips `module_info/0,1` — beamr-loadable by proof, NOT OTP-loadable by design | resurface at BC-5 only if artifacts are ever advertised OTP-loadable; BC-3's emitter may skip module_info |
| 13 | 2026-07-11 | BC-0 behavior oracle CLOSED at merge review: capstone Deliverable A re-run over the BC-0-built 44-module closure — normalized trail byte-identical to the pre-BC-0 baseline, and the BC-1 re-encoded copy matched in the same run (BC-0×BC-1 integration proven) | operator hands; also noted: the gleam compile proofs cover 12/51 valid fixtures (pre-existing test design) — BC-4's differential corpus supersedes them as the coverage instrument |
