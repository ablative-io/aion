# AWL program — execution plan

Status: ACTIVE. Owner: Tom + Fable orchestration; execution via aion
workflows (dev_brief) driving norn in **driven mode** (always — transcripts
live in the ops console, messages injectable). Authored 2026-07-09.

Canonical documents (read in this order to reconstruct full context):
1. `AWL-0-SPEC-DRAFT.md` — the language: rev-0 as implemented, complete
   keyword inventory, AWL-1 sanctioned rev with ALL decisions ratified
   (2026-07-09). No open language questions remain except the engine
   continue-as-new threshold VALUE (deferred, blocks nothing).
2. `AWL-BC-DESIGN-DRAFT.md` — direct beamr bytecode emission; phases BC-0..5;
   spike folded into BC-1's capstone; all §10 decisions ratified.
3. `AWL-UX.md` — the authoring-experience north star (human + AI loops,
   error quality bar, operator view, anti-goals). Acceptance direction for
   every brief.
4. `examples/` — dev_brief.awl (rev-0, checks green), sized_fanout.awl +
   approval_gate.awl (AWL-1, aspirational), README.md (construct tables,
   verified gaps).
5. `briefs/` — the dispatch corpus (rich `-input.json` shape = the durable
   CONTRACT; the dev_brief workflow takes a translated `BriefInput` — see
   `briefs/README.md` for the verified contract + translation rule).
6. `BRIEF-CRAFT.md` — the accommodations ledger: what measurably improves
   norn-driven brief output quality, each entry pinned to a real run.
   UPDATE IT AT EVERY DISPATCH REVIEW (Tom's standing instruction,
   2026-07-09).

## Decision doctrine (Tom, 2026-07-09)

Design questions with a clear best answer get DECIDED with best-in-class
judgment and documented — not escalated. Tom is asked only for genuine
preference/values calls and deployment-target values (e.g. the CAN history
threshold). Everything must be clear, consistent, fully documented; no
halfway houses, no shortcuts: a construct lands complete
(parser + printer + checker + emitter + fixtures + goldens) or not at all.

## Readiness gates (before first dispatch)

1. **Commit the doc corpus** (this file, spec, BC design, UX, examples,
   briefs) to aion main — brief worktrees must see them.
2. **Deploy gate** — NOT yet run as of writing: aion 0.8.0 binary is
   rebuilt/installed (Tom); remaining: gleam builds for workflow projects →
   `aion package` per archive (BUILD FIRST — #234 stale-tree trap) → deploy
   remediation + dev-brief + awl-hello archives → rebuild + restart worker
   binaries (they must carry norn `aecae78`, the prompt-cache driven-mode
   fix — verified committed+pushed on norn main) → verify registration +
   one smoke dispatch in the ops console.
3. **Pipe-cleaner** — dispatch AWL0-REFAC-001 solo before going 2-wide.

## Brief inventory

Concurrency rule: max 2-wide, and never two front-end (aion-awl) briefs at
once — they collide in the same files; pair one front-end brief with one BC
brief. Worktree isolation always. Bar reminders for every brief:
stage explicit paths (never -A), `git diff --no-ext-diff`, package name is
aion-rs, NEVER trust the workflow's green — Fable/Tom re-runs gates by hand
before merge.

### Wave 0 — pipe-cleaner

**AWL0-REFAC-001** `aion-awl` module refactor — DECOMPOSED 2026-07-09 into
five sequential single-file sub-briefs after the monolithic brief failed
twice with opposite modes (run `672b43a4` fraud, run `a4b40d8a` honest
rollback; BRIEF-CRAFT entry 10). Four files over the 500-line limit:
emitter.rs 2043, parser.rs 1585, lib.rs 737, checker.rs 641. Shared
contract for every sub-brief: split into folder modules ≤500 lines with
REAL responsibility-named children (`include!`/part_NN = auto-reject);
`mod.rs` = declarations + re-exports only; **zero behavior change** —
public API identical, every existing test green UNMODIFIED (except where a
sub-brief explicitly owns a test file's bypass purge), round-trip goldens
byte-identical, clippy `-D warnings`, fmt. Each sub-brief carries a
mechanical scope-fence gate (git-diff exclude over its owned paths), the
Cargo.toml lints-pinned gate, and a scoped no-bypasses grep; the developer
purges the pre-existing bypass attrs in the files it owns (full inventory:
parser.rs:1 multi-line block, ast.rs:1 + printer.rs:1 missing_docs,
lib.rs ×4, tests/awl2_defects.rs, tests/parser_printer.rs,
tests/field_trivia_and_duplicates.rs). Sequenced smallest-first:

- **AWL0-REFAC-001a** checker.rs (641) → `checker/`. No pre-existing
  attrs; zero lib.rs changes needed (`mod checker;` resolves the folder).
  Pilot — proves the pipeline end-to-end. **LANDED 2026-07-09** (run
  `a96973cb`, first-pass accept, 4/4 lenses, merged `adb6e4cf` after
  operator hand-verification — see BRIEF-CRAFT.md validation note).
- **AWL0-REFAC-001b** lib.rs (737) → thin crate root + named modules;
  purges lib.rs's module_name_repetitions + 3× too_many_lines by doing the
  deferred work (module_name_repetitions may move to a DECLARED workspace
  policy iff a public rename would otherwise be forced — #38 precedent).
- **AWL0-REFAC-001c** parser.rs (1585) → `parser/`; purges the parser.rs:1
  allow block (docs written, unwrap/expect eliminated, long fns split).
- **AWL0-REFAC-001d** emitter.rs (2043) → `emitter/`.
- **AWL0-REFAC-001e** hygiene finale: ast.rs + printer.rs missing_docs
  purge, the three test files' expect_used/unwrap_used/format_push_string
  purge (tests return Result + `?`), wire `aion awl check` over every
  committed .awl fixture in CI (closes the blind spot that hid the
  bounded_cycle checker failures) — EXCEPT the two bounded_cycle fixtures,
  knowingly red until AWL1-004 lands typed child contracts; list them
  explicitly in an expected-failures note guarded against silent green,
  with a comment pointing at AWL1-004 (a rejected-run salvage of this test
  exists; see briefs/). The crate-wide zero-bypass gate goes green here
  and stays a permanent gate from then on.

001b..e are stamped out from 001a's template once the pilot lands clean.

### Wave 1+ — AWL-1 front end (task #241; sequential, one at a time)

Every brief: full vertical (grammar + AST + parser + printer canonical form +
checker + Gleam emitter lowering + fixtures + checker-run goldens + spec
cross-reference). Spec section named in each brief is the contract.

- **AWL1-001 types-as-schemas**: JSON-shaped multi-line type declarations
  (commas, trailing-comma canonical, 100-col single-line rule, tolerant
  parse), `///` doc descriptions (type + field level, load-bearing),
  `aion awl schema <file> [--type Name]` emitting JSON Schema draft 2020-12
  (records → object/properties/required; Option(T) → optional; enum →
  string enum once AWL1-002 lands; descriptions at every level). The
  derivation is a PUBLIC aion-awl library function (`schema_for_type`); the
  CLI is a thin wrapper over it — AWL1-015 and every other consumer reuse
  the same derivation, never a reimplementation. Highest value: unblocks
  model output contracts. FIRST after refactor.
- **AWL1-002 enums + match**: `type X = A | B | C` (payload-less),
  exhaustive `match`/`case` step construct (one-call arms, same-name/type
  bindings across arms), `case Some as x`/`case None` for Option. Schema
  emission: string enums.
- **AWL1-003 named arguments**: required named args in action/child calls;
  checker enforces exact name match; fix-it diagnostic lists declared params
  in order. Migrates ALL fixtures + examples (dev_brief.awl updates in the
  same brief — no split state).
- **AWL1-004 typed child contracts**: `child name(params) -> Type`
  declarations; `do child`/`spawn` must reference one; results first-class;
  fixes the two red bounded_cycle fixtures and removes their
  expected-failure carve-out from AWL0-REFAC-001's CI wiring.
- **AWL1-005 otherwise** (complement of nearest preceding same-binding
  `when`; checker rules per spec).
- **AWL1-006 sequential each**: `each x in xs in order` (in-order, first
  failure stops, remaining never start).
- **AWL1-007 parallel block** (heterogeneous join-all; distinct bindings;
  any failure fails the step after that arm's retries).
- **AWL1-008 race block** (first-wins; same binding name/type all arms;
  losers cancelled through engine cancellation).
- **AWL1-009 spawn** (fire-and-forget child; `as` is a check error).
- **AWL1-010 literal indexing** `xs[0]` (non-literal index rejected;
  out-of-range = runtime step failure with expression span).
- **AWL1-011 until-fresh-binding**: implement spec ruling — `until` sees the
  step's own fresh `as` binding (currently only rebinds work; first-time
  binding is wrongly rejected).
- **AWL1-012 Dir builtin**: implement the spec'd content-addressed snapshot
  handle type in the checker (+ schema mapping decision documented in the
  brief).
- **AWL1-013 polish sweep**: task #238 items 1,3–10 (sentinel leak, builtin
  shadowing, each+wait rejection, UTF-8 span columns, span gaps,
  over-parenthesization, test breadth, fmt `//`→`// ` trailing whitespace).
- **AWL1-014 schema file imports**: `type Name from "file.schema.json"` —
  nominal type loaded from an existing JSON Schema at check time; typing via
  the record-shaped projection (unsupported structural keywords are named
  check errors), constraint keywords preserved and re-emitted canonically;
  the file travels into the package content-addressed. Spec section
  "Referencing existing JSON Schema files (DECIDED 2026-07-09)" is the
  contract. Front-end brief; anywhere after AWL1-001.
- **AWL1-015 automatic schema plumbing** (Tom 2026-07-09: "there shouldn't
  be an additional step"): `aion package` embeds schemas for all
  contract-reachable types in the package manifest; server exposes them
  (start forms, #209); model-output-contract action dispatches carry the
  schema to the worker so norn's `--output-schema` is fed automatically
  (rides the #186 contract-verification seam). Depends on AWL1-001's public
  `schema_for_type` derivation. NOT a front-end-only brief — touches
  packaging, server API, and worker dispatch; treat it like a BC-track
  partner for pairing purposes.
- **HELD (needs Tom value ruling, blocks nothing):** engine implicit
  continue-as-new at a history threshold → then unbounded `repeat until`
  becomes legal (spec AWL-1 ruling 3).

### BC track (task #240; pairs with front-end briefs, 2-wide)

- **BC-0 helper hoist** (Gleam, `aion_flow` SDK): move workflow-independent
  helpers (JSON value codecs, map_*_error adapters, retry/backoff loop,
  decoder plumbing) from per-module generated code into the SDK as generic
  functions; emitted Gleam shrinks to pure glue; existing differential
  fixtures stay green. Dispatchable immediately after Wave 0.
- **BC-1 beamr encode** (beamr repo): writer mirroring loader/decode
  (chunks/compact/opcode/instruction/etf), cargo feature `encode`
  default-off; round-trip property tests over the erlc-built corpus;
  **capstone acceptance (the folded spike)**: encode one minimal hand-built
  workflow module → beamr loads + validates → calls aion_flow → completes a
  run e2e → event trail matches its Gleam-built twin. BC-2/3 GATED on the
  capstone; if it surprises (validator/ABI/opcode), amend AWL-BC doc first.
  Fable reviews the encode API design + the capstone artifact.
- **BC-2 MIR + lower** (aion-awl): CheckedDocument → MIR (Call/MakeClosure/
  CaseResult/Literal/Bind); MIR golden files per fixture. Fable designs the
  MIR shape first (small design review, not a full pass).
- **BC-3 select** (aion-awl): MIR → beamr Instructions; x/y register
  allocation; per-shape unit tests; emitted modules load + validate.
- **BC-4 differential harness**: every fixture through BOTH backends under a
  real engine; identical durable event trails (run-id normalized); ABI
  contract tests (representation table in AWL-BC §4). CI, forever.
- **BC-5 wire-in**: `aion awl emit --target beam` default; `aion package`
  compiles .awl natively; deterministic-bytes test (same source → same
  content hash, #218); beamr release (0.14.0) + aion dependency bump.

### Later (pipeline, after the above stabilize)

- **TS-001** tree-sitter grammar (oracle: every fixture parses to the
  canonical printer's shape). **LSP-001** LSP (diagnostics = the 7 checker
  classes; formatting = the printer). **#215** `aion run --watch`.
  Gleam-emit-target demotion decision executes after BC-5 soaks.

## Shared brief context (paste into every brief's resolved_context)

- Canonical docs list (§ top of this file) as reading list; the spec section
  named in the brief is the CONTRACT — implement it verbatim; discovered
  spec ambiguities are findings to report, never license to improvise.
- Coding standards: CLAUDE.md conventions — no `#[allow]`/`#[expect]`/
  `#[ignore]` bypasses, thiserror in libs, no unwrap/expect in library code,
  files ≤500 lines, mod.rs = re-exports only, no silent failures.
- Gate argv (run EXACTLY, in order, before reporting):
  mechanical fraud/scope gates (no-bypasses grep, Cargo.toml lints-pinned,
  scope fence) → `cargo fmt` →
  `cargo clippy --workspace --all-targets -- -D warnings` →
  `cargo test -p aion-awl` → `cargo test -p aion-cli`.
  The full `cargo test --workspace` suite is NOT a workflow gate while
  tasks #243/#244 (load-sensitive flakes in untouched crates) are open —
  it burned the cycle budget of both AWL0-REFAC-001 dispatches on reds
  that weren't the developer's (BRIEF-CRAFT entry 11). The operator runs
  the workspace suite by hand at merge review, always.
- Round-trip property is sacred: `parse ∘ print = id`,
  `print ∘ parse ∘ print = print`, byte-level, for every fixture including
  new ones.
- Review lenses: standard adversarial set PLUS a **spec-fidelity lens** —
  one reviewer reads the named spec section and checks the implementation
  against it clause by clause.

## State at last update (2026-07-09)

- aion main `8a1c3681` (stack alignment + #239 teardown fix) pushed; stack:
  beamr 0.13.0 / haematite 0.4.1 / liminal 0.2.3 live on crates.io.
- norn main `aecae78` (driven-mode prompt-cache fix) pushed.
- Doc corpus (spec/BC/UX/examples/plan) authored, ratified, UNCOMMITTED in
  the aion working tree pending the first-wave briefs joining the same
  commit.
- Deploy gate NOT run. First dispatch blocked on it.
