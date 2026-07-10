# AWL-2 build plan — front-end rebuild

Status: LIVE — executing as an ultracode multi-agent workflow (Tom's go,
2026-07-10). This document is the plan of record and the running decision
log; the workflow appends to the log as phases complete.

Spec of record: [AWL-2-SPEC.md](AWL-2-SPEC.md) (aion main `124b1825`).
Golden examples: [examples/rev2/](examples/rev2/) — extracted verbatim from
the spec; the corpus grows around them.

## Objective

Rebuild the `aion-awl` crate against the rev-2 surface: lexer, parser,
canonical printer, typechecker, schema derivation, and the Gleam-stopgap
emitter port, plus the `aion awl` CLI rewire and migration of the committed
example/fixture corpus. End state: aion main compiles and gates green with
the rev-2 front end and NO old-grammar surface anywhere; `aion awl
check/fmt/emit` work on rev-2 files; the emitted module for the migrated
awl_hello example compiles under `gleam build`.

## Scope

**In:** `crates/aion-awl` (all modules), `crates/aion-cli/src/awl.rs`,
`crates/aion-awl/tests/**` (fixtures + goldens rebuilt), `examples/awl-hello`
migration, this document's decision log.

**Out (explicitly):** AWL-BC bytecode emission (#240 — the canonical model
is its input; it consumes this work, it is not part of it), LSP/tree-sitter,
`aion new` scaffolding, worker scaffolding, visual editor, on-the-fly tier,
live deploy/run proof (my hands with Tom after merge — requires the queued
server/worker binary swap, which this build does not touch).

## Method

- **No compatibility parse.** The old grammar dies in this change; old
  fixtures are deleted or re-expressed, never kept alongside.
- **Fixtures first.** Phase 1 authors the rev-2 golden corpus (valid +
  deliberately-invalid with expected diagnostics) before any implementation.
  Every later phase's objective gate is the corpus.
- **Serial implementation, parallel judgment.** Implementation agents run
  one at a time in a single shared worktree (branch `awl2-front-end`);
  adversarial review panels (3 lenses: spec-fidelity, correctness,
  conventions) run in parallel against each phase's output, followed by one
  bounded fix round and a verification pass. Fixture authoring fans out in
  parallel (distinct files, single consolidating commit).
- **Fable on every judgment seat** (implementation, panels, fixes) per
  Tom's direction; the shared model default is the session model.
- **Gates per phase** (agent-run, then re-run by the operator's hands at
  merge): `cargo fmt` + porcelain check, `cargo clippy -p aion-awl
  --all-targets -- -D warnings`, `cargo test -p aion-awl`. Integration
  phase adds `-p aion-cli` and the full workspace battery.
- **Merge bar:** nothing merges to main until every gate is re-run bare by
  the coordinating session's own hands on the integrated branch; the
  workflow itself never merges or pushes to main.
- Known flake provenance protocol applies (#243/#244/#248): isolated re-run
  once; green → proceed, recorded.

## Phases

1. **Fixtures** — fan out per construct family: (a) header/inputs/outcomes +
   shorthand types + enums; (b) schema doors (inline `schema {…}`, file
   import) + `?` optionality; (c) worker/action/child/spawn declarations;
   (d) step bodies: calls, `->` bindings, pipes, combinators, `wait`/`sleep`;
   (e) `after` DAG + fork/join (collection, named-branch, `sequential`);
   (f) loop/`counting` + conditional outcomes/routing + `on failure` +
   substep grammar (parse-level). Each family delivers valid fixtures AND
   invalid fixtures with expected-diagnostic annotations. A consolidator
   audits coverage against the spec's keyword inventory and commits.
2. **Lexer** — token tables for the rev-2 inventory (`->`, `|>`, `?`,
   keywords, duration/list/record literals, `//!` / `///` doc lines as data,
   `.field` accessors), spans source-correct (the AWL-0 defect regression
   suite's discipline carries over).
3. **Parser + canonical printer** — one phase, one property:
   `parse ∘ print = id`, `print ∘ parse ∘ print = print`, comments and doc
   lines lossless. Grammar per spec; ast.rs rebuilt around the unified
   anatomy (inputs/outcomes, DAG edges, fork groups, loop nodes with
   language-owned counters).
4. **Typechecker + schema derivation** — contracts on calls (required named
   args), binding flow along the graph, `after`/route target existence, no
   unreachable steps, route-cycle boundedness, outcome exhaustiveness
   (`otherwise` / enum totality), guard-dependent optionality
   (`is present`), `?` ↔ `required` in both schema directions, explicit
   null rejected everywhere, imported-schema projection rules carried over.
5. **Emitter port + CLI** — the Gleam-stopgap emitter re-targeted at the
   rev-2 canonical model; `aion awl check/fmt/emit` rewired; emitted
   fixtures compile under real `gleam build` (existing compile-proof test
   pattern). DAG semantics map to the Gleam SDK; the mapping decisions are
   recorded in the decision log by the implementing agent.
6. **Integration** — migrate `examples/awl-hello` (rev-2 .awl + regenerated
   module), delete every old-grammar artifact, full workspace battery,
   completeness critic (what's missing vs the spec — anything found feeds a
   final bounded fix round), decision-log consolidation into this document.

## Design decisions locked before the build (from the 2026-07-10 session)

| # | decision | rationale |
|---|---|---|
| D1 | No compat parse; rebuild clean | one grammar, one truth; old surface has one committed example |
| D2 | Fixtures authored before implementation | objective per-phase gate no agent can talk past |
| D3 | Loop counters are language-owned (`counting`) | survey fix 1: worker-carried counters rot silently |
| D4 | `?` optionality; explicit null invalid everywhere | survey fix 2; closes null-vs-Option permanently |
| D5 | Outcome types carry promised evidence | survey fix 3; outcome types ARE the handoff contract |
| D6 | Combinators (filter/map/sort/count) in-language, fixed vocabulary | worker round-trip for a filter is absurd; pure ⇒ replay-safe |
| D7 | `race` dropped from rev-2 | `wait … timeout` covers signal-or-deadline; no fixture demands first-wins |
| D8 | Emitter stays Gleam-stopgap in this build | AWL-BC (#240) consumes the canonical model separately |
| D9 | Doc comments are the one prose mechanism (`about` dead) | one load-bearing mechanism, models know it |

## Decision log (appended during the build)

Format: `| date | phase | decision | rationale |` — entries added by the
workflow's implementation agents and consolidated at integration.

| date | phase | decision | rationale |
|---|---|---|---|
| 2026-07-10 | lexer (fix round) | Inline `schema {` switches the lexer into raw capture: the brace-balanced body (string-aware counting, braces included) becomes ONE `SchemaBody` token, verbatim, exempt from all AWL lexical rules; JSON validation belongs to the parser | the spec's paste-verbatim door must accept legal JSON Schema the AWL token grammar cannot express (negative numbers, `1e-3`/`1E5` exponents, `\uXXXX`/`\/` escapes, foreign indentation), and raw bytes keep re-emit lossless |
| 2026-07-10 | lexer (fix round) | The raw door is `schema {` on one line; a brace on the NEXT line lexes as an ordinary `LeftBrace` for the parser to refuse | canonical shape is same-line; a positional trigger keeps capture deterministic |
| 2026-07-10 | lexer (fix round) | Doc-line classification is positional: `///` / `//!` trailing code lex as trivia `Comment`, never `DocLine`/`DocHeader` | the spec defines doc LINES; mid-line doc data would attach an author's trailing note to the NEXT declaration and break whole-line round-tripping |
| 2026-07-10 | lexer (fix round) | `Span.column` is character-based (bytes stay byte-true in `start`/`end`); `Newline` spans cover the full physical terminator incl. a stripped `\r`; indentation jumping more than one level is a lexical error | compiler-quality diagnostics: editor-correct columns after multibyte prose, no off-by-one anchors on CRLF files, no phantom `Indent` structure the source doesn't contain |
| 2026-07-10 | lexer (fix round) | Corpus size correction: the rev-2 corpus was 157 `.awl` fixtures at the lexer phase (not "214+"), 158 after `inline_verbatim_constraints.awl`; the corpus gate now asserts ≥158 | phase records must not overstate coverage; the tightened guard catches silent fixture loss |
| 2026-07-10 | parser | Contextual "soft" keywords: `filter`/`map`/`sort`/`count`, `node`/`timeout`/`retry`/`every`/`backoff`, and `empty`/`present`/`absent` are keywords only in their own grammar positions (combinator stages, config lines, `is` predicates) and act as ordinary names wherever a name is expected; structural keywords stay reserved everywhere | the corpus pins `count` as a field name and `retry` as an outcome-arm name; reserving the whole vocabulary would reject the spec's own examples. Narrows the spec's "keywords reserved everywhere" prose — flagged for reconciliation before ratification |
| 2026-07-10 | parser | Outcome-clause layout follows the spec's worked examples over the Canonical-formatting prose: a payload-constructing `route out(…)` ALWAYS breaks after the guard comma onto its own line one level deeper (even under 100 columns); a bare route stays on the guard's line when it fits. Group alignment (type-brace/`=` columns, header-outcome `type`/`route` columns within a run) IS canonical and its padding is width-exempt | the prose ("payload construction breaks after `route` when over 100 columns") and the worked examples (90/99-column payload clauses broken before `route`) disagree; byte-identity with the flagship pins the examples' reading. Recorded at the parser/printer phase in tests/fixtures/rev2/MANIFEST.md; spec prose flagged for reconciliation before ratification |
| 2026-07-10 | parser (fix round) | A stage-less bind chain (`head -> name`) never wraps: the printer keeps it on one line at any width | the break rule is "break before each `\|>`" and a stage-less bind has no `\|>`; a wrapped `-> name` continuation line is output the parser rejects (parse∘print=id violation) |
| 2026-07-10 | parser (fix round) | Loops declare `until` and `max` at most once; a duplicate line is a parse error anchored at the second keyword. A body statement after an outcome clause is likewise a parse error ("outcome clauses close the step") | silent overwrite/reorder made `aion awl fmt` drop or restructure user source; mirrors the duplicate config-key guard |
| 2026-07-11 | checker (fix round) | Named-branch fork branches walk in isolated clones of the pre-fork scope; branch bindings merge into the step scope only at `join`; `join -> name` on the named form is a check error | bare-fork branches run in parallel — a sibling's binding is never guaranteed mid-fork; the spec's named form joins bindless |
| 2026-07-11 | checker (fix round) | The route-cycle SCC includes `after` edges alongside route and fall-through edges | a dependency's completion re-arms its dependents, so a backward route plus a forward `after` edge is as unbounded as two routes; the narrower SCC recorded at the checker phase contradicted "unbounded cycles are unwritable" |
| 2026-07-11 | checker (fix round) | Every non-terminal falling-through step must have its completion consumed: the next step's fall-through edge or an `after` dependent; the file-final step still requires an explicit route | the successor duty ("every non-terminal step has a successor") was enforced only for the last step in the file — a stranded middle step checked clean |
| 2026-07-11 | checker (fix round) | A piped route (`… \|> route <target>`) must target a workflow outcome; steps, sibling substeps, and parent outcome arms are refused | the spec defines the pipe-route terminator as "the piped value is the payload"; steps carry no payloads, so the threaded value vanished silently |
| 2026-07-11 | checker (fix round) | Steps and workflow outcomes share one route-target namespace: a step named like a workflow outcome is a declaration-time error (anchored at the step, second-occurrence convention) | `route <name>` silently resolved to the step, so a run intending to finish transferred control instead |
| 2026-07-11 | checker (fix round) | A binding inside a collection-fork branch never counts as a loop's threaded-value rebind (the loop frame records its fork depth) | branch bindings do not escape the branch, so the loop would carry its seed forever while the checker believed it rebound |
| 2026-07-11 | checker (fix round) | Inline schema-door diagnostics anchor by walking the raw JSON body to the failing path (properties/items/`$defs`-ref navigation), never by first-occurrence token search | nearly every real schema repeats `"type"`; nested refusals anchored at the innocent root keyword — a systematic span lie |
| 2026-07-11 | checker (fix round) | Structural compatibility replaces its depth cap (>24 ⇒ accept) with a coinductive in-progress set of named pairs | the cap converted deep mismatches into silent acceptance; coinduction keeps recursive types terminating without accepting anything unproven |
| 2026-07-11 | checker (fix round) | Dead control flow is refused: statements after an unconditional body `route`, and outcome clauses on a body ending in an unconditional route; call-site config lines on CHILD calls are refused (`node`/`timeout` pin worker actions only) | advisory hardening from the same panel — dead code made the fall-through graph unsound, and the engine routes children, not a queue |
| 2026-07-11 | checker (fix round) | RECORDED, not enforced (flagged for spec reconciliation before ratification): a loop-carrying step with zero outcome clauses keeps the permissive reading (exhaustion falls through as the implicit outcome); `[T?]` in list-element position stays checker-accepted while schema derivation drops the element `?` | both need a spec ruling; enforcing either silently would invent surface the spec has not settled |
| 2026-07-11 | emitter | DAG mapping: dependency-connected steps group into *regions* (one entry each — the workflow start or a route target), each region lowers to one Gleam function running its steps in topological layers, `route <step>` is a tail call to the target region's function, substeps lower to sibling functions, and bindings thread between functions as parameters computed by a liveness fixed-point over the (cyclic — backward routes) call graph | the plan's proposed topological-layer mapping, adopted; generated Gleam is structured code, so regions/tail-calls are the shape conditional routing can take |
| 2026-07-11 | emitter | Refused shapes (spanned `EmitError`, all `AWL-BC #240` candidates): a route-targeted step WITH `after` dependencies; a routing step with `after`-dependents; a step routing away while parallel/upstream work in its group is outstanding; dependency groups with two entries; routes to mid-chain steps; substeps outside one trailing block at depth one; substep parents without outcome clauses; `on failure` combined with a body-terminal route. The route-target+`after` join is a shape the spec explicitly defines ("its `after` dependencies, if any, must also be complete") — flagged for reconciliation before ratification | the sequential region lowering cannot express these joins; refusing loudly beats lowering them wrongly, and the spec-defined shape among them needs a spec-level ruling on stopgap scope |
| 2026-07-11 | emitter (fix round) | Heterogeneous parallelism is REAL, not sequentialized: named-branch forks and single-call dependency layers with differing actions dispatch on ONE `workflow.all` of raw wire-unified activities — each branch rides an `Activity(String, String)` wrapper twin whose input is pre-encoded with the action's own input codec (byte-identical wire), and the join decodes each payload with its action's return codec (`awl_decoded`). The 2026-07-11 panel confirmed the prior sequential fallback silently violated "bare `fork` is parallel" on the spec's own worked examples | the SDK's `workflow.all` is homogeneous in both type parameters and decodes every payload with the FIRST member's output codec; an identity `Codec(String)` shared by all members is the one unification that survives that decode path |
| 2026-07-11 | emitter (fix round) | RESIDUAL mapping limit: a dependency layer whose members are more than single bare action calls (e.g. release_pipeline's `build_all` fork beside `notes`) still runs member-by-member in written order, and the generated module carries an `// awl stopgap:` comment naming the degradation at the site | the SDK parallelizes activities, not statement sequences — there is no heterogeneous task primitive to compose full step bodies; the comment keeps the limit non-silent until AWL-BC lowers the real graph |
| 2026-07-11 | emitter (fix round) | A `route` inside a `loop` body is refused at emit with a spanned error (statement, pipe-terminal, and nested forms alike). The checker keeps the spec-level walk (the shape stays checkable) — flagged for reconciliation before ratification: the spec says a route "ends the run", and the generated tail-recursive loop function has no early-exit channel, so the stopgap must refuse rather than emit a discarded no-op route (or worse, a mid-loop region call) | the panel's reproducer checked clean, emitted, compiled under `gleam build`, and looped straight past its route; refusal is the only honest stopgap answer, and whether the LANGUAGE forbids the shape is a spec ruling AWL-BC's real early-exit lowering should inform |
| 2026-07-11 | emitter (fix round) | Loop `max` is loop-invariant: the checker types it in the PRE-loop scope (loop-locals get a targeted diagnostic) and the emitter independently refuses a `max` referencing names unbound before the loop | `max` renders once at the loop call site, where loop-locals do not exist; the checker previously typed it in body scope, so `max round.budget` checked clean and emitted Gleam that could not compile — and the spec's "expression over inputs/bindings" reads pre-loop anyway |
| 2026-07-11 | emitter (fix round) | One type per binding name, workflow-wide: the binding pass refuses (spanned, at the second bind) a name bound with two different types in disjoint branches, instead of first-wins | the binding→type map is name-keyed and feeds region parameter annotations; first-wins annotated the other branch's parameters with the wrong type and emitted non-compiling Gleam from a checked-clean document (the checker degrades the conflict to Unknown) |
| 2026-07-11 | emitter (fix round) | Float ordering comparisons render Gleam's Float operator family (`<.` `<=.` `>.` `>=.`), keyed off either operand typing Float; Int pairs keep the bare operators | Gleam's bare ordering operators are Int-only; the checker admits Float/Float ordering, so the bare rendering failed `gleam build` on a checked-clean document (no corpus fixture had a Float comparison — `float_threshold_guard.awl` now pins it with a compile proof) |
| 2026-07-11 | emitter | Collection forks lower one unbound action call per item (`workflow.map`, or `list.try_fold` for `sequential`); literal indexing is refused inside parallel fork branches and outcome guards (contexts that cannot host the fallible-index prelude) | SDK `map` is the exactly-once-per-item primitive and takes one activity constructor; richer branch bodies and prelude-hosting are AWL-BC surface |
| 2026-07-11 | emitter | Retry mapping carried from the proven AWL-0 emitter, flagged for spec reconciliation before ratification: `retry N` → `RetryPolicy(max_attempts: N, …)` (N TOTAL attempts, so `retry 2` = one retry) and `backoff D..D` → `Exponential(initial: D, multiplier: 2.0, max: D)` — the 2.0 multiplier is an invented constant | the spec never says whether N counts retries or attempts and names no backoff curve; one spec sentence settles each, and silently changing the proven AWL-0 wire behavior mid-stopgap would be worse than recording it |

## Risks

- **Emitter DAG mapping** is the largest unknown (arbitrary `after` graphs +
  conditional routes onto the Gleam SDK's structured constructs). Mitigation:
  topological-layer mapping as the default proposal; the implementing agent
  records the chosen mapping and its limits as decision-log entries; compile
  proofs are the gate.
- **Gleam-toolchain load-sensitive flakes** (#248) can red-herring the
  compile proofs — provenance protocol applies.
- **Checker scope creep** — the checker is the product, but route-cycle
  boundedness and flow-typing are bounded to exactly what the spec states;
  anything beyond is a recorded follow-up, not silent scope.

## After this build

Live proof with Tom (deploy + run a rev-2 workflow end-to-end — needs the
queued server/worker swap), then the UX tail in order: `aion new` scaffold,
LSP over the real checker, tree-sitter highlighting, `scaffold-worker`,
`aion build`, visual editor, on-the-fly tier. The wave ladder in
AWL-EXECUTION-PLAN.md is superseded by this plan for the front end; AWL-BC
(#240) proceeds against the rev-2 canonical model.
