# AWL program — dispatch corpus (Wave 0 / Wave 1 / BC track)

Four dispatch briefs for the aion `dev_brief` workflow (norn-driven, always in
driven mode). Format: the established `-input.json` shape (template reference:
`~/Developer/ablative/haematite/docs/design/core/briefs/PERF-003-input.json`).
Authored 2026-07-09 at the AWL program kickoff. Canonical program plan:
`../AWL-EXECUTION-PLAN.md`; language contract `../AWL-0-SPEC-DRAFT.md`;
bytecode contract `../AWL-BC-DESIGN-DRAFT.md`.

## The briefs

- **`AWL0-REFAC-001-input.json`** (aion, `main`, 6 requirements) — the AWL
  program's pipe-cleaner. Splits the four over-limit `aion-awl` source files —
  `emitter.rs` (2043 lines), `parser.rs` (1585), `lib.rs` (737), `checker.rs`
  (641) — into folder modules each ≤500 lines, `mod.rs` = declarations +
  re-exports only, at ZERO behavior cost: the crate-root public API is
  byte-identical, every existing test passes UNMODIFIED, every round-trip and
  Gleam golden is byte-identical. It also wires `aion awl check` over every
  committed `.awl` fixture in CI (closing the blind spot that let the
  `bounded_cycle` checker failures hide), carving out exactly the two
  `bounded_cycle` fixtures as guarded expected-failures pointing at AWL1-004 —
  not fixed, not deleted, not silenced. `depends_on: []`.

- **`AWL1-001-input.json`** (aion, `main`, 6 requirements) — types-as-schemas,
  the first and highest-value AWL-1 construct. Implements the three ratified
  rules of the spec section *"Type declarations: JSON-shaped, described,
  schema-emitting (DECIDED 2026-07-09)"* verbatim: (1) JSON-shaped, comma-
  separated `field: Type` layout with the deterministic 100-column single-vs-
  multi-line canonical form and a tolerant parse; (2) `///` descriptions on the
  type and each field as load-bearing data; (3) `aion awl schema <file>
  [--type Name]` emitting JSON Schema draft 2020-12 (records → object /
  properties / required, Option(T) optional, `///` → description). Full
  vertical: grammar + AST + parser + printer + checker + Gleam emitter lowering
  + fixtures + goldens. `depends_on: ["AWL0-REFAC-001"]`. Front-end brief.

- **`BC-0-input.json`** (aion, `main`, 6 requirements) — the AWL-BC keystone
  (§2). Hoists every workflow-independent helper (JSON codecs, `map_*_error`
  adapters, retry/backoff loop, decoder plumbing) out of per-module generated
  Gleam and into the `aion_flow` SDK as generic functions, so an emitted module
  becomes pure glue and the bytecode instruction-selection surface collapses to
  ~10 node shapes. Lands as a pure win for the existing Gleam path; the
  existing differential fixtures are the gate. Touches the Gleam SDK
  (`gleam/aion_flow`) and the emitter goldens, not `aion-awl` internals —
  `depends_on: ["AWL0-REFAC-001"]` only so the emitter goldens it regenerates
  are stable. NOT a front-end brief (pairs 2-wide with one front-end brief).

- **`BC-1-input.json`** (**beamr**, `main`, 6 requirements) — the `.beam`
  writer (AWL-BC §3) plus the folded-spike capstone (§6). Adds an `encode`
  cargo feature (default-off; aion enables it) mirroring `loader/decode`
  (container / chunks / compact / opcode+instruction / ETF) and reusing the
  decode-side types, proven by `encode(decode(x)) == x` round-trip over the
  corpus, per-shape `decode(encode(ir)) == ir` unit tests, and the
  `loader/validate.rs` oracle. The mandatory capstone: hand-build one minimal
  workflow module's IR, encode it, load + validate it in beamr, call
  `aion_flow`, complete a run e2e, and match the Gleam-built twin's durable
  event trail. BC-2/BC-3 are gated on the capstone. `depends_on: []`
  (independent of the aion briefs). Gate argv is the **beamr** variant
  (`cargo fmt` → `cargo clippy --workspace --all-targets -- -D warnings` →
  `cargo test --workspace`; plus `--features encode`).

## Dispatch protocol

- **`workspace_id` = a fresh UUID minted at dispatch time.** Every file carries
  the placeholder `"MINT-AT-DISPATCH"` so nobody reuses a stale UUID — replace
  it with a newly minted UUID at the moment of dispatch, never before.
- **Pipe-cleaner goes SOLO first.** Dispatch `AWL0-REFAC-001` alone, before
  anything else, to prove the pipeline end-to-end on a change with a crisp,
  machine-checkable success condition. Do not go 2-wide until it lands.
- **Dispatch ≤2-wide.** After the pipe-cleaner, at most two briefs run
  concurrently, always with worktree isolation.
- **Never two front-end (aion-awl) briefs concurrently.** They collide in the
  same files. Pair one front-end brief (e.g. `AWL1-001`) with one BC brief
  (`BC-0` or `BC-1`). Of these four, `AWL1-001` is the only front-end brief;
  `BC-0` and `BC-1` are safe partners for it.
- **Bar reminders (every brief).** Stage explicit paths (never `git add -A`);
  inspect with `git diff --no-ext-diff`; the package name is `aion-rs`; NEVER
  trust the workflow's own green — Fable/Tom re-run the gates by hand before
  merge.
- **Reviewers.** `["Waffles the Terrible"]` on every brief; BC-1 additionally
  gets Fable on the encode API design and the capstone artifact. Review lenses
  are the standard adversarial set PLUS a spec-fidelity lens (one reviewer
  reads the named spec section clause by clause).
- **Readiness gate.** Per the plan, first dispatch is blocked on the deploy
  gate (aion 0.8.0 binary + gleam builds + `aion package` per archive + worker
  restart carrying norn `aecae78` + a smoke dispatch) and on committing the doc
  corpus (this file included) to aion `main`.

## Wave map (from `../AWL-EXECUTION-PLAN.md`)

- **Wave 0 — pipe-cleaner:** `AWL0-REFAC-001` (aion-awl module refactor + CI
  fixture-check wiring). SOLO first.
- **Wave 1+ — AWL-1 front end (task #241; sequential, one at a time):**
  `AWL1-001` types-as-schemas (FIRST after refactor), then `AWL1-002` enums +
  match, `AWL1-003` named arguments, `AWL1-004` typed child contracts (removes
  the `bounded_cycle` carve-out), `AWL1-005` otherwise, `AWL1-006` sequential
  `each`, `AWL1-007` parallel block, `AWL1-008` race block, `AWL1-009` spawn,
  `AWL1-010` literal indexing, `AWL1-011` until-fresh-binding, `AWL1-012` Dir
  builtin, `AWL1-013` polish sweep. HELD: engine implicit continue-as-new
  (needs Tom's value ruling; blocks nothing).
- **BC track (task #240; pairs with front-end briefs, 2-wide):** `BC-0` helper
  hoist, `BC-1` beamr encode + capstone, `BC-2` MIR + lower, `BC-3` select,
  `BC-4` differential harness, `BC-5` wire-in. BC-2..5 are sequential behind
  BC-1's capstone.
- **Later (pipeline):** `TS-001` tree-sitter grammar, `LSP-001` LSP, `#215`
  `aion run --watch`; Gleam-emit-target demotion after BC-5 soaks.

Only `AWL0-REFAC-001`, `AWL1-001`, `BC-0`, and `BC-1` are authored as dispatch
briefs in this directory; the rest of the wave map is the forward plan.
