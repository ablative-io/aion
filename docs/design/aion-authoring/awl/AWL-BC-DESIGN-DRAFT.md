# AWL-BC — direct beamr bytecode emission for AWL: design draft

Status: DRAFT for ratification (Tom). Tracker: task #240. Supersedes the
model-interpreter execution tier (#216) if it lands. Authored 2026-07-09.

## Position

AWL-0 compiles today via a stopgap: `aion awl emit` generates Gleam source,
which requires the Gleam compiler and an Erlang/OTP install (`erlc`) on the
authoring machine to become the `.beam` modules beamr executes. AWL-BC removes
that entire toolchain from the workflow author's path: the typechecked AWL AST
is lowered directly to `.beam` bytecode by aion's own Rust code.

The Gleam emitter does not go away. It becomes the **reference
implementation** and the differential oracle: every AWL program compiled
through both paths must produce identical durable event trails. Whether it
remains a user-facing `emit` target or becomes test-only machinery is an open
decision (§10).

What this buys:

- **Zero third-party toolchain.** Authoring a workflow requires exactly one
  binary: `aion`. No Gleam, no OTP, no erlc. `.awl → .beam` happens in
  milliseconds inside `aion package` / `aion run`.
- **One wall of errors, not two.** Today a `.awl` file that passes `awl check`
  can still surface Gleam-compiler errors pointing at *generated* code. Under
  AWL-BC the backend is total for checked programs: if `awl check` passes,
  emission cannot fail. Every diagnostic the author ever sees is span-anchored
  to their own source.
- **Deterministic bytes.** We control every emitted byte, so the same `.awl`
  produces the same `.beam` bit-for-bit — directly fixing the package
  content-hash nondeterminism family (#218) for AWL workflows.

## 1. Architecture

```
.awl text ──parser──► AST ──checker──► CheckedDocument
                                           │
                          ┌────────────────┴───────────────┐
                          ▼                                ▼
                 Gleam emitter (reference)          AWL-BC backend
                   emitter.rs, unchanged        lower ─► select ─► assemble
                          │                                │
                    gleam build + erlc                     │
                          ▼                                ▼
                       .beam  ═══ differential oracle ═══ .beam
                          (identical durable event trails)
```

Three stages, three modules (new, inside `aion-awl`; the assembler lives in
`beamr` — §3):

1. **`lower`** — CheckedDocument → a small mid-level IR (MIR): a flat list of
   functions, each a tree of `Call` / `MakeClosure` / `CaseResult` /
   `Literal` / `Bind` nodes. This is where every AWL construct maps to its SDK
   call shape, mirroring the Gleam emitter's lowering decisions 1:1. The MIR
   is the layer the differential tests pin.
2. **`select`** — MIR → beamr `Instruction` sequences: register allocation
   (x-registers for args/scratch, y-registers across calls), stack frames
   (`allocate`/`deallocate`), tail-call positions (`call_ext_last`), pattern
   tests (`is_tuple`/`get_tuple_element`/`select_val`/atom equality),
   closures (`make_fun2` + FunT entries).
3. **`assemble`** — instructions + atoms + literals + imports/exports/funs →
   the `.beam` container (§3).

## 2. The keystone: shrink the emitted surface first (AWL-BC-0)

The current Gleam emitter generates substantial *workflow-independent* helper
code per module — JSON value codecs, `map_activity_error` / `map_timer_error`
/ `map_receive_error` / `map_child_error` adapters, the retry loop
(case + `workflow.sleep` backoff), decoder plumbing. Textual codegen makes
that cheap; bytecode codegen makes it expensive and pointless.

**AWL-BC-0 is a refactor of `aion_flow` (Gleam, pipeline-able): hoist every
workflow-independent helper into the SDK as generic functions.** After it, an
emitted module — in either backend — is pure glue:

- module-level literals (step names, `about` prose, durations, codec configs),
- external calls into `aion_flow`,
- closures wrapping step bodies (for `workflow.map`, `with_timeout`, retry),
- Result pattern matches routing to handler blocks.

This cuts instruction selection from "reimplement a chunk of a Gleam code
generator" to roughly ten node shapes. It also lands as a pure win for the
existing Gleam path (smaller generated modules, one canonical retry/codec
implementation) and can be dispatched through the norn pipeline immediately —
it needs no bytecode knowledge, and the existing differential fixtures gate it.

## 3. The `.beam` writer lives in beamr (`encode`)

beamr already defines the entire format in Rust, decode-side:
`loader/parser.rs` (IFF container), `loader/decode/chunks.rs` (AtU8 / Code /
ImpT / ExpT / LocT / LitT / FunT…), `loader/decode/code.rs` (the Code chunk's
instruction stream — the writer must mirror this too, it is where the emitted
instructions actually live), `loader/decode/compact.rs` (compact term
operand encoding), `loader/decode/opcode.rs` (the ~99-opcode supported set
with arities), `loader/decode/instruction.rs` (typed `Instruction`, ~129
variants), `loader/decode/etf.rs` (external term format for literals), and
`loader/validate.rs` (load-time validation).

The writer is the mirror image and should sit next to the reader as a beamr
`encode` module (feature-gated if we want to keep the default build lean):

- shares the chunk/operand/instruction types — no duplicated format knowledge;
- **round-trip property tests for free**: `encode(decode(x)) == x` over every
  `.beam` in the existing corpus (the compiled `aion_flow` tree gives hundreds
  of real erlc-produced modules), and `decode(encode(ir)) == ir` for
  generated ones;
- `validate.rs` becomes the first-line oracle: emitted modules must load and
  validate through the same path erlc output does;
- beamr gets a capability with value beyond AWL (test fixture synthesis,
  future JIT/replay tooling).

Constraint carried everywhere: **emit only within beamr's supported opcode
subset.** Not limiting in practice — erlc-compiled Gleam already lives inside
it; that is why beamr runs the current output.

## 4. The ABI contract (AWL-BC-ABI)

The emitted module calls compiled-from-Gleam code, so it must speak Gleam's
Erlang representation exactly. This gets pinned as a documented, tested
contract — a fixture module in the differential suite asserts each row:

| Gleam value            | Erlang term the bytecode must produce            |
|------------------------|---------------------------------------------------|
| `String`               | UTF-8 binary                                      |
| `Int` / `Float`        | integer / float                                   |
| `Bool`                 | `true` / `false` atoms                            |
| `List(a)`              | proper list                                       |
| `Option(a)`            | `{some, V}` / `none`                              |
| `Result(a, e)`         | `{ok, V}` / `{error, E}`                          |
| custom type record     | tagged tuple `{tag_atom, F1, …, Fn}`              |
| `fn(…) -> …`           | fun (FunT closure or external fun)                |
| module `aion/workflow` | atom `aion@workflow` (Gleam module-name mangling) |

Also part of the contract: the exported entry point (`definition/0` returning
the `workflow.WorkflowDefinition` record, whose fields include closures), the
x-register calling convention, and arity conventions for the SDK functions we
call. A Gleam-version bump that shifts any representation must fail these
contract tests loudly — that is the point of writing them down as tests
rather than lore.

## 5. Instruction-selection surface (post-AWL-BC-0)

The complete set of MIR shapes `select` must handle:

1. module preamble: `func_info` / labels / export table entries
2. literal loads (from LitT) and atom loads
3. `call_ext` / `call_ext_last` into `aion_flow` and `gleam/*` stdlib modules
4. local calls between generated functions (`call` / `call_only`)
5. closure creation (`make_fun2`/`make_fun3` + FunT) for step bodies,
   `each` bodies (→ `workflow.map`), timeout bodies, retry bodies
6. Result matching: `is_tuple` + `get_tuple_element` + atom `is_eq_exact`
   (or `is_tagged_tuple` if beamr supports it — spike verifies), `select_val`
   for multi-arm handlers
7. record/tuple construction (`put_tuple2`), list construction (`put_list`)
8. stack discipline: `allocate`/`deallocate`/`trim`, y-register spills across
   calls
9. `jump`/labels for handler routing
10. `return`

No arithmetic beyond what expressions in the AWL keel need, no receive loops,
no process primitives — the SDK owns all of that.

## 6. Phases, oracles, ownership

| Phase | What | Oracle | Who |
|-------|------|--------|-----|
| **BC-0** | Hoist workflow-independent helpers into `aion_flow` (§2) | existing differential fixtures stay green; generated-Gleam diff shrinks | pipeline |
| **BC-1** | beamr `encode` module (§3) — **capstone acceptance criterion (the folded spike, ruled by Tom 2026-07-09):** encode one minimal hand-built workflow module; beamr loads + validates it; it calls `aion_flow` and completes a run e2e with an event trail matching its Gleam-built twin. BC-2/BC-3 do not start until this capstone is green — the proof is mandatory, only the standalone spike dispatch is deleted | round-trip over the erlc corpus; `validate.rs` accepts; the capstone run | pipeline (Fable reviews the API design + the capstone artifact) |
| **BC-2** | MIR + `lower` from CheckedDocument | MIR golden files per AWL fixture | Fable designs MIR; pipeline implements |
| **BC-3** | `select` + register allocation | emitted modules load + validate; per-shape unit tests | pipeline, Fable review |
| **BC-4** | Differential harness: every AWL fixture through BOTH backends under a real engine; identical durable event trails; ABI contract tests (§4) | the trails, byte-for-byte after run-id normalization | pipeline |
| **BC-5** | Wire in: `aion awl emit --target beam`, `aion package` does `.awl → .beam` natively, deterministic-bytes test (#218) | `aion package` twice → identical content hash; e2e deploy + run | pipeline |

Corpus for BC-4: every existing AWL fixture plus adversarial cases — `each`
over an empty list, `each` over a runtime-sized list, nested Result handlers,
timeout-inside-retry, unicode in `about` and string literals, maximum-arity
records, a workflow with zero steps, child-workflow spawn.

Sequencing: BC-0 and BC-1 run in parallel from the start; BC-2..5 are
sequential behind BC-1's capstone. The capstone retains the spike's
falsification role: if it surprises us (validator rejection, ABI mismatch,
missing opcode), this document gets amended before BC-2/BC-3 move — the
mechanical core of BC-1 (round-trip writer) is needed under any design
outcome, so nothing is wasted by the surprise.

## 7. What stays exactly as it is

- **The language.** AWL-BC changes no syntax, no semantics, no spec text.
  The sanctioned language rev (`otherwise`, `match`/enums, `each` over child
  workflows) is a separate track; it lands in the front end and both backends
  pick it up through `lower`.
- **The checker as the single gate.** All seven check classes keep running
  before either backend. Emission remains typecheck-gated.
- **Workers.** Activities are still implemented in any language against the
  worker SDK. AWL orchestrates; it never grows IO vocabulary.
- **The runtime module set.** The compiled `aion_flow` SDK (and the `gleam/*`
  stdlib modules it needs) still ships as prebuilt `.beam` artifacts inside
  the package. The Gleam toolchain moves from every-workflow-build to
  SDK-release-time only.

## 8. Authoring experience: end state

The target loop, for a human or an AI author, on a machine with nothing but
the `aion` binary:

```
$EDITOR onboard_customer.awl        # one plain-text file; `about` prose is
                                    # load-bearing (canvas label, console
                                    # narration), not a comment
aion awl check onboard_customer.awl # instant, span-anchored diagnostics;
                                    # the ONLY error surface that exists
aion awl fmt onboard_customer.awl   # the canonical rendering (one true
                                    # formatting; AI diffs stay minimal)
aion run onboard_customer.awl --input @cust.json --watch   # (#215) compile
                                    # in ms, deploy to the dev server, stream
                                    # the run; edit → re-run on save
aion package && aion deploy         # same bytes every time (#218 fixed)
```

Properties worth naming, because they are the "clean and intuitive" ask made
concrete:

- **Check-clean means it runs.** No second compiler to appease, no errors
  pointing at code the author never wrote. For AI authors this makes
  `awl check` a machine-checkable gate a pipeline can loop on mechanically.
- **Milliseconds, not toolchains.** The edit→run loop has no gleam/erlc
  stage, no dependency fetch, no build cache to go stale (#234's trap class
  shrinks with it).
- **Determinism is structural, not disciplinary.** The language still has no
  clock/random/IO vocabulary — an AI cannot write a nondeterministic workflow
  even by accident; the only world-touching verb remains `do`.
- **The file reads as a runbook.** `about` narration surfaces in the console
  and canvas, so the artifact the author writes is the artifact the operator
  reads during an incident.
- **Tooling rides the same keel.** The tree-sitter grammar and LSP
  (sanctioned, pipeline track) get diagnostics from the same checker and
  formatting from the same printer — every surface tells the author the same
  thing.

## 9. Risks

| Risk | Mitigation |
|------|------------|
| beamr's opcode subset or validator rejects a shape we need | BC-S spike exists to hit this in week one; worst case we add the opcode to beamr (we own it) — a bounded, testable change |
| ABI drift on a future Gleam/SDK version bump | §4 contract tests fail loudly; SDK artifacts are version-stamped in the package |
| Divergent semantics between backends (the plausible-but-wrong class) | BC-4 differential trails are the gate for every fixture, in CI, forever; the Gleam path never gets deleted while it is the oracle |
| Register-allocation bugs (subtle, runtime-visible) | post-BC-0 the surface is glue code with shallow expressions; per-shape unit tests + validator + differential trails triple-cover it |
| Scope creep toward "general Gleam compiler" | the emitted-surface inventory (§5) is closed; anything not on it goes into `aion_flow` as SDK code instead |

## 10. Decisions (ratified by Tom, 2026-07-09 — recommendations accepted)

1. **AWL→Gleam emit-target visibility.** This decision is ONLY about the
   machine translation of `.awl` files into Gleam source. It is NOT about
   Gleam as an authoring language: hand-writing workflows in Gleam against
   the `aion_flow` SDK remains a first-class, fully supported option for
   authors who want full control, permanently and independently of AWL-BC
   (ratified intent, Tom 2026-07-09). The question is whether
   `aion awl emit --target gleam` stays user-facing or becomes test-only
   oracle machinery once bytecode soaks. Recommendation: demote after BC-5;
   no user needs machine-generated Gleam once `.awl` compiles direct, and
   escape hatches that exist get depended on.
2. **beamr `encode` feature gating.** Always-on module vs `encode` cargo
   feature. Recommendation: feature, default-off; aion enables it.
3. **Where MIR lives.** Private to `aion-awl` vs a documented public layer
   other tools may target. Recommendation: private until a second consumer
   is real.
4. **beamr version policy.** BC-1 lands in beamr and needs a release (0.14.0)
   before aion can consume it from crates.io — or aion path-deps during
   development and we release once at BC-5. Recommendation: the latter;
   one clean release.
