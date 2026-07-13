# AWL-BC-IR — the ratified MIR design (D-AOT2 contract document)

Status: **RATIFIED** — BC-2 design of record, 2026-07-12. Produced by the
plan's BC-2 method row (competing designs + judge panel): three competing MIR
designs (`template-first`, `typed-flow-first`, `continuation-first`) were
adjudicated by a three-judge panel. **`template-first` won (2 of 3 first-place
rankings)** and is the base of this document; every judge-recommended graft is
either applied or explicitly rejected with a reason in §1.

Parent: `AWL-BC-BUILD-PLAN.md` (decisions 1–13 honored without exception:
D-BC1 rev-2-only input, D-BC3 parity-first refusals, D-AOT1 sidecars,
D-AOT2 this document, decision 9 hoist-only codec templates, decision 12 no
`int_code_end` / no `module_info`). Worked codec reference:
`AWL-BC-CODEC-DESIGN.md` (§2 `Desc` shape reused as pure lowering-time data).
Evidence base: `AWL-BC-1-CAPSTONE-EVIDENCE.md` (observed entry ABI
`run/1 → {ok, ResultBinary}`, obs. 8; instruction alphabet of Deliverable B;
writer-contract observations 2–7).

Operator amendment honored (2026-07-12): **beamr 0.14.0 IS PUBLISHED and
includes `loader/encode`.** BC-3 consumes `beamr = "0.14.0"` with feature
`encode` from crates.io — no path-dep anywhere; D-BC4's worktree-only patch
discipline is satisfied vacuously, and the plan's BC-5 "beamr 0.14.0 release"
step is already done.

Maintenance rule (D-AOT2): any BC-3/BC-4/BC-5 change touching the node set
(§2–§3), a lowering rule (§4), a capability entry (§6), a contract row (§7),
or a decision-register entry (§8) lands **in the same commit** as the code
change, enforced at panel review. MIR stays private to `aion-awl` (ratified
decision 3).

### BC-2 implementation status (2026-07-13)

This records the state of the `crates/aion-awl/src/mir` implementation against
the design above, replacing the code comments' references to a "BC-2 report"
that was never in the tree. It is the honest covered/pending split for the
current increment; each pending item names why it is not yet landed.

**Landed and golden-pinned:** the MIR node set (§2.5/§2.7), `lower` for the
covered subset (multi-step sequential regions; action calls incl.
action-declared retry/timeout/task_queue/node config; sleeps; action/field
pipes; routes; success/failure outcome returns; ordered `when`/`otherwise`
`If` tails; enum-total `SelectEnum` tails; short-circuit `and`/`or`/`not`
decision trees; `Cmp` guards; `is present` `AssertSome` narrowing; and
**bounded loops** — `FlowFn(Loop)` per the §4 row, skeleton-reserved slots
after every chain slot, `Increment` + untagged `TupleNew` for counted
results, the loop `until` reusing the outcome guards' short-circuit decision
builder), record `_to_json` for required leaf/`Ref` fields, the
codec-composer trio, the T-DEF/T-RUN/T-EXEC/T-ACT/T-SIG template shells,
`project_sidecar` (S2, pinned against the SDK type spellings — `SignalRef` in
`aion/signal`, `WorkflowDefinition` in `aion/workflow/define`), `verify`
(capability closure + runtime/local-call arity + single-def), and **S14
backward liveness** (`lower/liveness.rs`).

**Loop semantic decisions (BC-2b-3, pinned by test — the spec is silent or
the checker/emitter is the law):**

- **Re-entry resets.** A backward route that re-enters a loop-carrying step
  re-evaluates the seed and calls the loop function with count 0 — the
  reference emitter's exact behavior (each entry to the step emits the loop
  call anew). The exam ledger records single-assignment-under-re-entry as
  UNSTATED spec (F-family) and the bounded-cycle rule as unsound-in-spirit
  under reset; BC pins the implementation, not an invented spec
  (`backward_route_reentry_resets_seed_and_count`).
- **Counted result ABI = untagged `TupleNew`.** `Ok(#(value, count))` needs a
  dynamic 2-tuple; `RecordNew` would inject a tag atom at element 0 and break
  the `TyDesc::Tuple`/Gleam ABI. The closed op set gains `TupleNew{dst,items}`
  (§2.5) with printer/verify/liveness/selection support; the call site
  destructures with `FieldGet` 0/1 (untagged ⇒ 0-based).
- **`FnOrigin::Loop.index` is module-global** — the pre-order ordinal across
  the whole document (regions in plan order, statements pre-order, a loop
  numbered before its body), matching the stopgap's monotonic `loop_counter`
  naming `<step>_loop_<n>`.
- **`until` uses the guard decision builder.** Gleam value-position `&&`/`||`
  short-circuit, so an eager `BoolOp` lowering of `until` would diverge on an
  effectful/narrowing RHS — the BC-2b-2 short-circuit bug family. One builder
  (`outcome::lower_condition`), no second lowering path. The nested
  `loop_compound_until_nested` fixture covers both `and` and an
  optional-narrowing `or`; its MIR test pins continuation cloning only into
  unresolved leaves, and the emitter test independently pins Gleam source
  order.
- **Counterless result ABI is scalar.** `loop_without_counting` pins the legal
  no-`counting` path: `Result(value, AwlError)`, one call-site `TryBind`, no
  `TupleNew`, and no `FieldGet(0/1)` pair destructure. The corpus-wide select
  oracle loads and validates this path alongside the counted tuple path.
- **Non-positive `max` still runs one pass** (post-test, matching the
  stopgap and the spec's at-least-once rule); ceiling exhaustion exits `Ok`
  and the step's mandatory conditional outcomes distinguish it
  (`loop_lowering_pins_the_reference_semantics`).
- **Defensive scope matches the checker.** A named counter is removed from the
  emitter's cloned loop-body scope and `until` refs are validated before
  rendering; direct unchecked emit therefore cannot leak a post-loop counter
  into the generated function. A `counting` name equal to the threaded binding
  is checker-illegal, avoiding the stopgap's single-name/single-type map and
  Gleam's duplicate-pattern restriction.
- **Chain-boundary liveness has one explicit asymmetry.** The boundary-specific
  collector follows shared `collect_loop` seed/max/local handling, but does not
  register loop-body binds as step defs; the shared region collector does so as
  an aggregation artifact. Checked documents cannot read those binds after the
  loop, and keeping them local is the stricter boundary behavior.
- **BC-4 runtime obligation.** Selection currently proves `TupleNew` and
  `Increment` by load+validate. The differential runtime oracle must execute a
  counted loop and pin first observable count 1 plus the ceiling-pass value;
  structural BC-2/BC-3 evidence does not claim execution coverage.

**Pending increments (NOT yet lowered; each needs infrastructure beyond a
bounded fix-round, and — per the design — must be blessed against the BC-4
differential oracle, not frozen as un-executed goldens):**

- **Codec `_decoder` bodies (record/enum/union) and enum/union `_to_json`
  (§3).** The decoder recipe is continuation-taking: it needs the lifted-closure
  function inventory (§2.6 item 8, `FnOrigin::LiftedClosure`) and a slot-plan
  extension reserving `3 + Σ(lifted)` slots per codec type. Until landed, the
  `_decoder` bodies are visible `decode.success(nil)` placeholders and
  enum/union `_to_json` are `json.object([])` placeholders — structurally
  present in goldens, never silently correct.
- **D4 optional-field `_to_json` omission and composite (list/option) fields.**
  The reference `[pair]`/`[]` + `gleam@list:flatten` recipe requires MIR ops the
  closed set does not yet carry (a dynamic JSON-pair-list constructor + flatten
  path); records with optional or list/option-typed fields therefore keep the
  current static-`json.object` body. A follow-up either adds those ops or routes
  such records through `LowerError::Unsupported`.
- **Composite (list/option) codec trios (§3).** Their `_to_json`/`_decoder`
  are single-call (no lifted closures) but add codec types to the registry;
  they land with the decoder increment so the whole codec surface re-baselines
  once.
- **T-DEAD / T-ACTRAW / T-WIT shells.** T-ACT's expansion references a
  `make_fun2(T-DEAD)` dead body (§2.4); the shell lands with BC-3's T-ACT
  expansion so the FunT inventory and sidecar carry it.
- **`verify` per-op result-type cross-check against the rev-2 `TypeEnv` (S1).**
  Not reachable from `verify(&MirModule)` (no `TypeEnv` in the signature);
  performed in BC-3, which holds the environment during instruction selection.
- **MIR visibility (decision 3).** Held at `#[doc(hidden)] pub mod mir` until
  the pending increments construct the currently-unused variants (they are
  `dead_code` under `pub(crate)` + `-D warnings` today); tightened to
  `pub(crate)` once the op surface is fully constructed.

---

## 1. Synthesis decisions

The winning `template-first` design is amended as follows. "S" entries are
applied grafts; "X" entries are judge-suggested elements deliberately
rejected. Each cites its origin design.

### Applied

- **S1 — `verify(&MirModule)` pass** (from typed-flow-first; judges 1+2).
  A verifier runs inside every MIR golden test: capability closure (every
  callee is a `RuntimeFn` variant), local-call arity checks against the
  function list, single-def variable discipline, tail-position invariants
  (every `Block` ends in exactly one `Tail`; no statement follows a
  terminator), and per-op result-type cross-checks against the rev-2
  `TypeEnv` at the op's source span. Template-first as submitted had no
  verifier — capability/arity errors would have surfaced only at BC-3.
- **S2 — sidecar as a projection of the MIR** (from typed-flow-first; all
  three judges). The `.gleam_types` bytes are `project_sidecar(&MirModule)`,
  a pure fold over the finished function list — the `sidecar: Vec<SidecarSig>`
  field is **removed** from `MirModule`. One source, one ratchet, two
  artifacts: a sidecar regression is definitionally a MIR regression. The
  `MirType→TypeDescriptor`-style mapping is **total** (every `TyDesc` arm
  maps, §5), including `run/1`'s `Dynamic` parameter and type parameters on
  `Custom` descriptors.
- **S3 — float literals carry the source lexeme** (from typed-flow-first;
  judges 1+2). `MirLiteral::Float(f64)` becomes `Float { lexeme: String }` —
  the emitter already carries float literals as source lexemes
  (`exprs.rs:151`, `Expr::Float { value } → value.clone()`), and byte-stable
  `LitT` floats require pinning the parse. A BC-4 fixture row asserts the
  emitted float bytes equal the reference's parse of the same lexeme.
- **S4 — value-position booleans completed** (from typed-flow-first; judge 1
  defect 1). The submitted sketch's prose/enum mismatch is closed: the `Stmt`
  set gains `BoolOp { dst, op: And|Or, lhs, rhs }` and `Not { dst, src }`, so
  `flag: !x` and `a && b` in argument/record-field position (the
  `render_arg_for` path, `exprs.rs:190-267`) are expressible. Test-position
  short-circuiting remains nested `If` tails; value-position `&&`/`||` lower
  as a test + `move true/false` materialization burst (no new imports).
- **S5 — union-decoder zero value is op-built, never a `LitT` claim** (from
  typed-flow-first; judges 1+2 defect). The submitted "the zero is pure data
  stored as `LitT`" claim was wrong for `zero_expr`'s
  `duration.milliseconds(0)` arm (`types.rs:171` — not literal-expressible).
  Ratified rule: the decoder-failure zero is built by **ordinary MIR ops**
  inside the expanded decoder function's fallback arm (a `CallRt DurationMs`
  for the duration arm, `RecordNew`/`Bind` for the rest). The
  recursive-required refusal (`types.rs:174-183`) is preserved verbatim.
  BC-3 may pool a fully-constant zero into `LitT` as an encoding choice; the
  MIR never asserts poolability.
- **S6 — `FnOrigin` provenance on every MIR function** (from
  typed-flow-first; judges 1+3). Richer than name-scheme-only identity:
  `Run | Definition | Execute | Region | SubStep | Loop | ActivityWrapper |
  SignalRef | CodecTemplate{kind, params} | LiftedClosure{host, index} |
  DeadBody | ChildWitness`. Feeds goldens, sidecar ordering, and BC-3 review.
- **S7 — the `nil_codec` output-codec fallback stated as a rule** (from
  typed-flow-first; judge 1 defect 4 / graft 6). When the workflow has no
  success outcome, T-DEF/T-RUN reference `awlc.nil_codec` as the output codec
  (`frame.rs:157-163`, verified). `CodecRef::SdkNil` existed in the sketch;
  the rule is now explicit and is contract row IR-21. The T-DEF recipe is
  also corrected: **3 codec calls + the workflow-name binary** (name, input
  codec, output codec, error codec, execute — `frame.rs:176-186`), not "4
  codec calls".
- **S8 — codec trios expand into ordinary MIR functions at LOWER time**
  (judge 2's decisive graft, from typed-flow-first; supported by judge 3 and
  by continuation-first D6). The four `CodecTrio` template shapes remain
  template *shapes* (decision 9 stands — the recipes of §3.6 are stamped from
  `TypeEnv`), but they are stamped **during `lower`**, producing ordinary
  `FlowFn`s carrying `FnOrigin::CodecTemplate { kind, params }` provenance —
  where `params` retains the descriptor-style template parameters
  (`WireDesc` field/arm specs per `AWL-BC-CODEC-DESIGN.md` §2), so a post-BC
  descriptor-engine revisit swaps codec bodies without touching `lower`.
  Consequences: every decoder-continuation lambda exists as a `Lifted` flow
  function with an explicit capture list **before BC-3 runs**; the complete
  FunT population and all capture chains are golden-visible; `select` never
  synthesizes a function. This deletes template-first's one select-time
  function-synthesis site (its worst defect under the selectability lens)
  while keeping the recipes exactly as specified. The former
  `--expand-templates` dump mode is the **default and only** golden form.
  The **shells stay opaque template nodes** (T-DEF, T-RUN, T-EXEC, T-ACT,
  T-ACTRAW, T-SIG, T-DEAD, T-WIT): they are name-substitution-only, mint at
  most one 0-free `make_fun2`, and are safe to expand in `select` from fixed
  recipes.
- **S9 — closure sidecar arity = physical BEAM arity** (from
  typed-flow-first risk 7; judges 1+3). Lifted-closure sidecar rows record
  declared params + appended captures — the FunT arity the JIT actually sees;
  the source arity is recoverable from the `Fn` descriptor at the
  `MakeClosure` site. Promoted from a risk note to contract row IR-22.
- **S10 — `Custom` descriptor module-string spellings pinned before goldens
  freeze** (from typed-flow-first risk 2; judge 3). The exact module-string
  vocabulary (`gleam/option`, `gleam/json`, `gleam/dynamic`, `aion/duration`,
  SDK modules) is pinned against the `gleam-types` extractor's own output for
  compiled `aion_flow` as a BC-2 action, and is contract row IR-23.
- **S11 — effect-schedule-first golden printing** (from continuation-first;
  judges 1+2). Every MIR golden prints, as its FIRST section, the park-point
  schedule: the durable `CallRt` ops (`aion@workflow` define/run/all/map/
  spawn/spawn_and_wait/receive/with_timeout/sleep) in order with their
  wire-visible arguments. Trail-affecting diffs lead every review; the BC-4
  oracle's spine is visible before instruction selection exists.
- **S12 — the adjudicable-decision register** (from continuation-first;
  judge 1). All deliberate deviations and their pre-authorized fallbacks live
  in one table (§8): R1 flattened `TryBind` (fallback: `gleam@result:try/2` +
  continuation), R2 `Concat` via `gleam@string:append/2` (fallback:
  bs-op binary construction, only if corpus-proven), R3 `decode.string`
  Decoder-constant materialization pinned by disassembling the
  reference-compiled corpus during BC-3 — never guessed (all three designs
  independently flagged this; ratified once here), R4 Line chunk from MIR
  spans (cut without ceremony if it drags BC-3).
- **S13 — `degraded_parallel` metadata marker** (from continuation-first;
  judges 1+2). Regions containing a multi-statement dependency-parallel layer
  lowered in written order (`steps.rs:169-180`) carry a
  `degraded_parallel: true` marker, printed in goldens — the stopgap
  degradation is visible in MIR, not only as a comment in the Gleam twin.
- **S14 — `live_after` annotations, printed in goldens** (from
  continuation-first; judges 1+2). BC-2 computes backward liveness per
  function and annotates every `CallRt`/`CallLocal`/`CallClosure`/`TryBind`
  op with the set of vars live across it — the y-spill contract handed to
  BC-3 as data, not a computation. Printed in goldens, so regalloc-relevant
  changes surface as visible MIR diffs instead of silent BC-3 behavior
  shifts. Register allocation itself stays in BC-3.
- **S15 — the "notably absent" negative-space section** (from
  continuation-first; judge 3). The capability manifest (§6) enumerates what
  can NEVER appear, not just what may.
- **S16 — durable-operation classification as derived metadata** (from
  continuation-first's EffectKind angle; judge 3 graft 5). The golden printer
  (and an exported per-module summary for the beamr AOT track) classifies
  `CallRt` ops into durable families — timers / activities / children /
  signals used — giving a semantic capability manifest at park-point
  granularity layered above the flat import list. This is **derived** from
  the `RuntimeFn` callee, never a bespoke MIR node vocabulary (see X6).
- **S17 — `If` and `SelectEnum` promoted from `Stmt` to `Tail`** (from
  continuation-first's terminator discipline; judge 2 graft 6). In every
  source shape these constructs are terminal (outcome cascades end in routes,
  loop bound checks end in exit/recurse, enum-total cases end in routes), so
  the type system now enforces that control constructs end blocks — the
  unreachable-trailing-statement ambiguity is unrepresentable. No lowering
  changes. `WaitTimeoutCase` and `Attempt` remain fused value-producing
  statements: their interior arms merge back into the continuation by fixed
  recipe, which is exactly the emitter's shape (`stmts.rs:292-313`,
  `steps.rs:301-354`).
- **S18 — ImpT determinism stated as a contract row** (template-first's own
  rule, promoted per judge 3 graft 3): the emitted ImpT chunk is exactly the
  used `RuntimeFn` subset, in **first-use order** (row IR-24).

### Rejected

- **X1 — continuation-first's sidecar "safe-by-omission" rule.** REJECTED
  (all three judges concur). Omitting closures and dropping `run/1` entirely
  is silent erasure in the one artifact whose reason to exist is no-erasure
  (D-AOT1); `run/1`'s `Dynamic` parameter is representable
  (`CustomType{gleam/dynamic, Dynamic}` — typed-flow-first proved the mapping
  against the verified format). Every function gets a row (§5).
- **X2 — typed-flow-first's basic-block CFG with typed block parameters
  (SSA/phi).** REJECTED (judge 2 defect 2). The reference emitter only ever
  produces continuation-nested trees; a general CFG admits join shapes the
  reference never generates, forcing BC-3 to build a general linearizer with
  no corpus twin — or assert tree-ness, in which case the generality bought
  nothing. Control flow stays a **tree per function**; `jump`/label emission
  is mechanical.
- **X3 — typed-flow-first's full interior typing (TypeTable interning, a
  `TypeId` on every value edge).** REJECTED as MIR representation. The
  fidelity D-AOT1 needs is delivered without carrying per-edge type ids
  through `lower`/print: function signatures carry total `TyDesc`s (the
  sidecar projection source, S2), and `verify` cross-checks op result types
  against the `TypeEnv` at source spans (S1). Full interior typing is real
  machinery purchased for zero selectability benefit (judge 2 defect 3); if
  `TypedRegister.type_index` emission is ever wanted, the signature layer +
  `TypeEnv` still contain everything needed to add it additively.
- **X4 — typed-flow-first's `Concat` = binary-construction (bs-op family)
  as the primary choice.** REJECTED as unproven: no corpus evidence yet that
  beamr's encoder/validator supports that family for our shape, and
  `gleam@string:append/2` is trail-identical and trivially safe. Kept as the
  registered fallback/alternative in R2 — a flip changes a marked row, never
  introduces a surprise import.
- **X5 — continuation-first's `OutcomeFail`-as-terminator.** REJECTED
  (judge 2 defect 4). Folding `json.to_string(to_json(payload))` into a
  terminator couples value evaluation into terminator selection. Failure
  outcomes remain ordinary `CallRt` ops (`to_json`, `JToString`) followed by
  `RecordNew` + `Return` — the one place continuation-first's own Op/
  Terminator boundary bent.
- **X6 — continuation-first's bespoke `EffectKind` node vocabulary** (12
  variants with bespoke structs). REJECTED as MIR representation: the
  largest bespoke surface of the three designs, with drift risk concentrated
  where templates and Effects meet (judge 1 defect 4). Its genuinely valuable
  semantic-capability angle is adopted as derived classification instead
  (S16), and its `live_after` discipline as S14.
- **X7 — typed-flow-first's `UnconsN` multi-result op.** REJECTED: "occupies
  count consecutive ids" breaks one-op-one-def discipline (judge 2 defect 4);
  `AssertList { binds, list }` already covers the join destructure as a
  single pinned burst.

---

## 2. The MIR

### 2.1 Position and central claim (ratified from template-first)

The rev-2 Gleam emitter (`crates/aion-awl/src/emitter/`) is a closed template
expander; the MIR is the catalog itself, reified:

- **Template shells** (T-DEF, T-RUN, T-EXEC, T-ACT, T-ACTRAW, T-SIG, T-DEAD,
  T-WIT) are single MIR nodes parameterized by names, atoms, and type shapes;
  BC-3 expands each from a fixed recipe (name substitution only — no lambda
  minting beyond a 0-free `make_fun2`).
- **Codec trios** are template *shapes* stamped **at lower time** into
  ordinary flow functions with `FnOrigin::CodecTemplate` provenance (S8).
- **Flow functions** (execute, regions, substeps, loops, lifted closures,
  expanded trio functions) have bodies drawn from a closed statement-op set
  where each op is one known instruction burst with no interior
  register-pressure decisions: single-def `Var`s, every op defines ≤1 fresh
  var (mirroring the emitter's `awl_piped_N`/prelude discipline).
- **No registers, no labels in MIR.** BC-3 owns x/y assignment,
  `allocate`/`deallocate`/`trim` frames, y-spill across calls (seeded by the
  S14 `live_after` sets), and label resolution.
- **Control flow is a tree per function, never a CFG** (X2). Nested blocks
  each end in a `Tail`.

Selectability grounding: the BC-1 capstone's hand-built module proved the
target instruction alphabet (`Label`/`FuncInfo`, `Allocate`, `Move`,
`CallExt`, `IsTaggedTuple`, `Deallocate`, `Return`) loads through all five
validation layers, survives the content-hash rename machinery, and produces a
trail identical to its Gleam twin (`AWL-BC-1-CAPSTONE-EVIDENCE.md`,
Deliverable B). Every other named instruction appears in beamr's typed
`Instruction` set (`loader/decode/instruction.rs`) and in the 44-module
corpus the capstone round-tripped without exclusions (Deliverable A), and is
encodable per `loader/encode/opcodes.rs`.

### 2.2 The one deliberate shape deviation: flattened `result.try` (R1)

`use x <- result.try(...)` sites lower to first-class `TryBind`, selected as
the flattened form:

```
call_ext <the fallible op>            ; then the error mapper call_ext
is_tagged_tuple Lfail, x0, 2, 'ok'
get_tuple_element x0, 1 -> <dst>
... continue in the same frame ...
Lfail:  ; x0 already holds {error, E}
deallocate N; return
```

Exactly the capstone Deliverable B shape — proven loadable, validate-clean,
engine-run, trail-identical. Error-propagation semantics are identical
(`result.try` returns the `{error, E}` term unchanged; so does the fail
branch). Buys: no closure explosion (a region with 8 statements emits 0
lambdas instead of 8), no environment-tuple churn, mechanically verifiable
frames. D-BC3 pins trails, not instruction bytes (capstone obs. 2: the
writer's bytes are never erlc's bytes). Fallback registered as R1 (§8).

### 2.3 Module

`MirModule` = logical name (snake of the workflow name), source file name,
export list (**exactly** `run/1`, `definition/0`, `execute/1`; no
`module_info` — decision 12 / capstone obs. 4), interned atom table, literal
pool (beamr `Literal` shapes only), the function list, and the `TypeShape`
registry. **No sidecar field** — the sidecar is a projection (S2).

Functions are `Templated(TemplateFn)` (shells only, post-S8) or
`Flow(FlowFn)`.

### 2.4 Template shells (BC-3-expanded; name substitution only)

| Shape | Instantiates | Gleam reference | BC-3 recipe |
|---|---|---|---|
| **T-DEF** `definition/0` | workflow name, codec refs | `frame.rs:165-189` | `make_fun2`(execute, 0 free) + workflow-name binary + **3 codec calls** (input, output-or-`SdkNil`, `awl_error.codec`) + `call_ext_last aion@workflow:define/5` (S7 correction) |
| **T-RUN** `run/1` | codec refs | `frame.rs:191-203` | `make_fun2`(execute) + 2 codec calls (input, output-or-`SdkNil`) + `call_ext_last aion@awl@runtime:run/4` |
| **T-EXEC** `execute/1` | input field list, entry region + params | `steps.rs:45-83` | `get_tuple_element` per input (element i+1), `call_only step_<entry>/n` |
| **T-ACT** `<action>_activity/n` | action name, input record, codec refs | `wrappers.rs:11-93` | `put_tuple2` input record (bare atom if zero-field), codec calls, `make_fun2`(T-DEAD), `call_ext_last aion@activity:new/5` |
| **T-ACTRAW** `<action>_activity_raw/n` | same + pre-encode | `wrappers.rs:100-149` | as T-ACT plus: `call` input codec, `get_tuple_element` its `encode` field, `call_fun` on the record — the pinned `Codec(a)`-is-a-record-of-funs dependency (row IR-11) |
| **T-SIG** `<signal>_signal/0` | signal name, payload codec | `wrappers.rs:151-168` | binary literal + codec call + `call_ext_last aion@signal:new/2` |
| **T-DEAD** dead-body lambda | message literal | `wrappers.rs:79-81` | lifted fn: `call_ext aion@error:terminal/1`, `put_tuple2 {error,_}`, `return`; FunT entry, 0 free |
| **T-WIT** child witness | fixed | `stmts.rs:18-19` | lifted fn: `put_tuple2 {awl_child_failed, <bin>}`, `put_tuple2 {error,_}`, `return`; FunT, 0 free |

`CodecTrio` is **no longer a function node** — see §3.

### 2.5 Flow functions and the statement-op set

`FlowFn` = `FnOrigin` (S6), name, params (`Var`s from `Plan.params` — the
liveness fixed-point output; or declared args + appended captures for
`Lifted`), param/return `TyDesc`s (the sidecar projection source), body
(`Block`), span, `degraded_parallel` marker (S13).

The complete op set — each row one instruction burst, each grounded:

| Op | Lowers (source) | Instruction burst |
|---|---|---|
| `Bind{dst,value}` | literals, refs, input prelude (`exprs.rs:142-214`) | `move` of var/literal/atom/int |
| `FieldGet{dst,base,index}` | `.field` access (`pipes.rs:169`, `exprs.rs:170-173`) | `get_tuple_element base, index` — **the MIR `index` is already the BEAM element index (1-based, tag at 0); `lower` stores `position+1` (`codec.rs:155`, `flow.rs:241`), so the burst does NOT add another `+1`** (D-BC3 correction, BC-3) |
| `RecordNew{dst,tag,args}` | record construction (`exprs.rs:291-341`), Some-wrap (`pipes.rs:257-270`), outcome payloads (`outcomes.rs:114-208`) | `put_tuple2`; zero-field ⇒ `move` of the bare tag atom |
| `TupleNew{dst,items}` (BC-2b-3) | counted-loop `Ok(#(value, count))` result (`loops.rs:88-94`) — untagged, so `RecordNew`'s tag atom cannot stand in | `put_tuple2` without a tag element |
| `ListNew{dst,items}` | list literals, `workflow.all` arg lists | `put_list` chain from nil, or `LitT` when fully constant |
| `CallRt{dst,callee,args}` | every SDK/stdlib call (§6) | `call_ext` |
| `CallLocal{dst,fn,args}` | wrapper/codec/loop-fn invocation | `call` |
| `CallClosure{dst,fun,args}` | T-ACTRAW encode; attempt invocation | `call_fun` (`instruction.rs:228`) |
| `MakeClosure{dst,lifted,captures}` | §2.6 sites only | `make_fun2`/`make_fun3` + FunT entry |
| `TryBind{dst,result}` | every `use x <- result.try(...)` site | flattened form §2.2 (capstone-proven); fallback R1 |
| `WaitTimeoutCase{dst,receive,captures,deadline_ms}` | `wait` + timeout (`stmts.rs:292-313`) | `make_fun2` + `call_ext with_timeout/2` + nested `is_tagged_tuple` over the 4 arms building `{ok,{some,V}}`/`{ok,none}`/errors |
| `Cmp{dst,op,lhs,rhs}` | value-position comparisons (`exprs.rs:194-267`) | `gc_bif`/test + `move true/false`; Int/Float split preserved |
| `BoolOp{dst,op,lhs,rhs}` (S4) | value-position `&&`/`||` | test + `move true/false` materialization |
| `Not{dst,src}` (S4) | value-position `!x` | `is_eq_exact 'false'` test + materialize |
| `Concat{dst,lhs,rhs}` | `<>` (`exprs.rs:266`) | `call_ext gleam@string:append/2` (R2) |
| `Increment{dst,src}` | loop counter (`loops.rs:88`) | `gc_bif2 erlang:'+'` small-int |
| `AssertList{binds,list}` | `let assert [a,b] = awl_layer` (`steps.rs:280`, `forks.rs:328,385`) | `get_list`/`get_hd`/`get_tl` + `is_nil`, fail → `badmatch` (unreachable by construction, emitted valid) |
| `AssertSome{dst,option}` | `is present` rebind (`outcomes.rs:350-373`) | `is_tagged_tuple {some,1}` + extract, fail → `badmatch` |
| `JsonObj{dst,pairs}` | child-input assembly (`stmts.rs:160-184`, `pipes.rs:236-239`) | per pair: to_json call + `put_tuple2` pair; `put_list` chain; `call_ext gleam@json:object/1` |
| `IndexGuard{dst,base,index,msg}` | `items[i]` prelude (`exprs.rs:174-185`) | `call_ext aion@awl@runtime:index/3` + TryBind burst |
| `Attempt{lifted,captures,defs,on_ok,on_err}` | `on failure` (`steps.rs:301-354`, `subs.rs:65-127`) | `make_fun2` attempt closure (body = Lifted fn ending `Ok(defs-tuple)`), `call_fun`, `is_tagged_tuple {ok,2}` → destructure into `on_ok`; else `on_err` (compensation, must end in a route — refusal preserved) |

`CallRt`/`CallLocal`/`CallClosure`/`TryBind` ops carry the computed
`live_after` set (S14). Every op carries a `Span` (→ Line chunk, R4).

### 2.6 The closed closure inventory

`MakeClosure` appears ONLY where an SDK/stdlib API takes a fun:

1. `workflow.map` item body (`forks.rs:256-261`)
2. `list.try_fold` folders — sequential/child forks (`forks.rs:154-246`)
3. `with_timeout` receive body (`stmts.rs:296`)
4. combinator accessors/comparators — `list.filter/map/sort` (`pipes.rs:93-147`)
5. `on failure` attempt closure (`steps.rs:316`)
6. T-DEAD dead body, T-WIT witness (0 free)
7. `execute` passed as a value in T-DEF/T-RUN
8. expanded-trio internals: the `json_codec` encode-fun ref and decoder
   continuations — now ordinary `Lifted` FlowFns per S8

Nothing else. This list is the FunT chunk's entire population, and post-S8
it is fully golden-visible.

### 2.7 Tails

`Return(value)` | `TailLocal{fn,args}` (→ `call_last`/`call_only`: routes,
region fall-through, loop recursion) | `TailRt{callee,args}` (→
`call_ext_last`: shells) | **`If{test,then,else}`** and
**`SelectEnum{subject,arms}`** (promoted per S17; each arm/branch is a
`Block` ending in a `Tail`; enum-total `select_val` fail label points at a
valid `badmatch` block). Outcome returns are `RecordNew` compositions then
`Return`: success `{ok, {Ctor, payload}}`; failure `{error,
{awl_outcome_failure, name_bin, json_string}}` with the
`json.to_string(to_json(...))` calls as ordinary `CallRt` ops before it (X5).

---

## 3. Codec-trio template shapes — stamped at lower time (decision 9 + S8)

The four shapes, their stamping recipes (grounded in `codecs.rs` /
`composites.rs` and the generated reference `awl_hello.gleam:109-213`); all
output is ordinary `FlowFn`s with `FnOrigin::CodecTemplate{kind, params}`:

- **Record trio** (`codecs.rs:149-266`) — `<stem>_codec/0`:
  `MakeClosure(<stem>_to_json ref, 0 free)` + `CallLocal <stem>_decoder/0` +
  `TailRt aion@codec:json_codec/2`. `to_json/1`: per field `FieldGet` +
  leaf/composite to_json + `RecordNew` pair + `ListNew`; optional fields
  contribute `[pair]`/`[]` arms and the list flows through
  `CallRt gleam@list:flatten/1` (`codecs.rs:180-209`) — D4 preserved
  mechanism-for-mechanism, byte order = declaration order. `decoder/0`: the
  decode API is genuinely continuation-taking, so n nested `Lifted` fns,
  lambda k capturing fields 1..k-1, each `CallRt decode:field/3` (or
  `optional_field/4` with `none` default + `decode.map(_, Some)` per
  `codecs.rs:236-249`), terminal `decode.success(RecordNew)`. Bounded:
  lambdas-per-module = Σ record fields + 2·unions + enums.
- **Enum trio** (`codecs.rs:268-306`) — to_json: `SelectEnum` tail over
  variant atoms → binary literals → `CallRt gleam@json:string/1`; decoder:
  `decode.then(decode.string)` with a lifted fn doing an `is_eq_exact`
  cascade against binary literals, arms `decode.success(atom)`, fallback
  `decode.failure(first_variant, name_bin)`.
- **Union trio** (`codecs.rs:70-146`) — to_json: `SelectEnum` on ctor atom →
  `RecordNew` pairs + `json.object`; decoder: outer
  `decode.field("outcome", decode.string)` continuation with string cascade,
  per-arm `decode.field("payload", <payload_decoder>)` continuation,
  fallback `decode.failure(<zero>, union_name)` — the zero built by ordinary
  ops per S5 (covers the `duration.milliseconds(0)` arm of `zero_expr`);
  recursive-required refusal preserved (codec design §3.3).
- **Composite trio** (`composites.rs:16-101`) — list: `json.array/2` +
  `decode.list/1`; option: `json.nullable/2` + `decode.optional/1`; inner
  refs resolved exactly as `Emitter::to_json_fn`/`decoder_fn` do (leaves →
  SDK `awlc.*`, named → module-local). Instantiation set = the same
  wire-position reachability walk (`codecs.rs:20-68`, `composites.rs:16-51`).

Template parameters reuse the `WireDesc` shape (`AWL-BC-CODEC-DESIGN.md` §2:
leaves, list, nullable, ref) as pure Rust data — no descriptor engine ships.
The `decode.string` Decoder-constant representation is pinned by R3, never
guessed.

---

## 4. Per-§5-shape lowering rules (CheckedDocument → MIR)

Lowering rule zero (D-BC1): `lower` consumes the SAME planning passes the
Gleam emitter runs — `build_env` (`types.rs:243`), `bindings::compute`,
`graph::plan` (union-find regions, single-entry validation, Kahn layering,
liveness fixed-point params; `graph.rs:150-377`, `liveness.rs`). BC-2
refactors these three passes out of `emitter/` into a shared crate-private
`plan` module consumed by both backends — region/layer/param decisions and
refusals CANNOT drift (the strongest parity lever; shared by both top-ranked
designs). `lower` is total for checked documents modulo the recorded
refusals (§4.1); any other failure is a bug, never a user-visible surface.

| Canonical-model shape | MIR lowering |
|---|---|
| workflow + inputs | T-EXEC (input `FieldGet`s from `Plan` first-region params, incl. the non-input-param error `steps.rs:59-79`), T-DEF, T-RUN, record trio for the input record |
| step region | one `FlowFn(Region)` per `Plan.regions[i]`, params = `Plan.region_params`, body = layers in order (`steps.rs:155-199` recursion → continuation nesting) |
| dependency-parallel layer, all single bare same-action calls | `ListNew` of activity values + `CallRt workflow.all/1` + `TryBind` + `AssertList` (`steps.rs:235-282`) |
| … heterogeneous single-call layer | T-ACTRAW raw twins + `workflow.all` + per-branch `CallRt awlc.decoded/3` + `TryBind` (`forks.rs:351-400`) |
| … fuller bodies | written order (sequential), region marked `degraded_parallel` (S13); refusal-free degradation preserved (`steps.rs:169-180`) |
| action call (`do`) | T-ACT instance + config `CallRt` pipes (`activity.retry/timeout/task_queue/node`, site > action config, task_queue always — `stmts.rs:84-104`) + `CallRt workflow.run/1` + `CallRt map_activity_error/1` + `TryBind` |
| child call / `spawn` | `JsonObj` input + T-WIT + `CallRt spawn_and_wait/6` or `spawn/6` + `map_child_error`/`map_spawn_error` + `TryBind` (`stmts.rs:160-266`); child-config refusal preserved |
| `wait` signal | T-SIG + `CallRt workflow.receive/1` + `map_receive_error` + `TryBind`; with timeout → `WaitTimeoutCase` (`stmts.rs:269-315`) |
| `sleep` | `CallRt duration:milliseconds/1` + `CallRt workflow.sleep/1` + `map_timer_error` + `TryBind(_)` — the literal capstone module's body |
| pipe chain | per stage a fresh `Var`: action stage (single-param, declaration config only), `.field` = `FieldGet`, combinator = `MakeClosure`(accessor/comparator) + `CallRt gleam@list:*` (`pipes.rs:152-179`); terminator bind/route belongs to the caller |
| collection fork (parallel) | `MakeClosure`(item body) + `CallRt workflow.map/2` + `TryBind`; branch-prelude-indexing refusal preserved (`forks.rs:249-253`) |
| collection fork (`sequential`) / child forks | `try_fold` folder closures + `list.reverse`; parallel children = spawn-fold then `child.await`-fold twin folds (`forks.rs:132-263`) |
| named fork | homogeneous → typed `workflow.all`; hetero → raw twins; single → plain call; non-call branches refused (`forks.rs:269-343`) |
| bounded loop | `FlowFn(Loop)` `<step>_loop_k`, params `(var, awl_count, awl_max, free…)` from `loop_free_names`; body then `Increment`, then `until`/bound checks as `If` **tails** (S17): exit `Return Ok(var)` or `Ok(#(var,count))`, recurse `TailLocal`; call site `CallLocal` + `TryBind` (+ tuple destructure) (`loops.rs:25-131`); unbounded/route-in-body/non-invariant-max refusals preserved |
| substeps | `FlowFn(Sub)` per substep; sibling routes = `TailLocal`; parent-arm firing re-enters the parent frame's outcome lowering; chain end = parent outcomes inline (`subs.rs`); all `substep_split` refusals preserved (`graph.rs:73-116`) |
| outcome clauses | enum-total → `SelectEnum` tail; else `If` cascade tail with `otherwise` terminal; guard-indexing refusal preserved; `is present` → `AssertSome` rebind (`outcomes.rs:215-373`) |
| route | outcome return (`RecordNew`+`Return`), region tail (`TailLocal`), sibling substep tail, or parent-arm resolution — the full `emit_route` decision order (`outcomes.rs:25-110`) |
| `on failure` | `Attempt` (defs-tuple protocol of `render_defs_tuple`, `steps.rs:360-372`); compensation route-tail requirement preserved |
| declared/projected types | `TypeShape` entries (tag atoms via `names::snake`, arity) + one trio stamping (§3) per record/enum/union/composite reachable from a wire position |
| keel expressions | `Bind`/`FieldGet`/`Cmp`/`BoolOp`/`Not`/`Concat`/`RecordNew`/`ListNew`/`IndexGuard` per `exprs.rs`; float lexemes retained (S3); Int/Float ordering split preserved (`exprs.rs:237-268`) |

### 4.1 Refusals (D-BC3 parity, verbatim set)

`lower` refuses exactly what the reference refuses, with the same spans and
message families, via the shared `plan` module wherever the refusal lives
there: route-targeted ∧ `after`-dependent step; routing step with
`after`-dependents; two-entry regions; mid-chain route targets; route-away
with outstanding parallel work; first-step-not-entry; substeps not a
trailing block / nested substeps / substeps without parent outcomes;
`on failure` with body-terminal route; unbounded loop; route in loop body;
non-loop-invariant `max`; indexing in outcome guards and parallel fork
branches; collection-fork bodies beyond one unbound call; named-fork
branches beyond action calls; child config pinning; `zero_expr`
required-field recursion (`graph.rs:203-346`, `steps.rs:73-116,301-311`,
`loops.rs:137-167`, `subs.rs:77-86`, `outcomes.rs:226-231,325-331`,
`forks.rs:48-295`, `pipes.rs:62-73`, `types.rs:174-183`). BC-4's oracle
intersection is unchanged by construction.

---

## 5. `.gleam_types` sidecar strategy (D-AOT1)

**The sidecar is `project_sidecar(&MirModule) -> Vec<u8>`** (S2): a pure fold
over the finished function list through a **total** `TyDesc →
gleam_types::TypeDescriptor` mapping, serialized by the published
`gleam-types` crate (v0.4.3 on crates.io; magic `GLEAM_TYPES\0`, version 1,
per-function `FunctionSignature { name, arity, param_types, return_type }` —
`gleam-types/src/format.rs`). No document walk, no second traversal, no
drift axis: sidecar goldens and MIR goldens share one source.

- **Mapping (total — every arm maps, no erasure, X1 rejected):**
  `Bool/Int/Float/Str/Nil` → 1:1 leaves; `List/Tuple/Result/Fn` structural;
  `Option(t)` → `CustomType{gleam/option, Option, [t]}`; named
  records/enums/unions → `CustomType{<this module>, Name}`; `Duration` →
  `CustomType{aion/duration, Duration}`; `Json` →
  `CustomType{gleam/json, Json}`; `Dynamic` →
  `CustomType{gleam/dynamic, Dynamic}`; `Codec/Activity/SignalRef/
  WorkflowDefinition/AwlError` → `CustomType` with their SDK module + type
  params (the format supports parameters — `format.rs:33-37`);
  `Unknown` → `List(Nil)` — `Unknown` arises only from empty list literals,
  so the value IS a list; the reference's `gleam_type` renders it bare `Nil`
  (`types.rs:116`), and the sidecar deliberately keeps the list shape (the
  judge-adjudicated more-faithful projection; typed-flow-first's treatment).
  Module-string spellings pinned per S10/IR-23.
- **Coverage: every function** — exports first (export-table order), then
  locals in canonical function order, then lifted closures (including all
  expanded-trio functions and decoder continuations, which post-S8 are
  ordinary functions with signatures). Example row: `run/1 :
  (CustomType{gleam/dynamic, Dynamic}) -> Result(String,
  CustomType{aion/awl/error, AwlError})` — the typed statement of the
  observed entry ABI (capstone obs. 8).
- **Closure rows use physical BEAM arity** (S9/IR-22): declared params +
  appended captures, matching the FunT lambda the JIT sees.
- **Determinism**: one canonical order, no timestamps, no absolute paths;
  bytes golden-tested per fixture (a BC-2 acceptance ratchet).
- **Handoff**: bytes land as `<module>.gleam_types` next to each `.beam`
  (the JIT read path exists: `jit/aot.rs` reads
  `beam_path.with_extension("gleam_types")`); archive entry +
  `load_companion_into_cache` plumbing is BC-5; runtime JIT consumption is
  post-BC (plan D-AOT1 verbatim). `TypedRegister.type_index` emission is
  deliberately not in BC-3 v1; the signature layer keeps it additive (X3).

---

## 6. Runtime-capability set (= the tree-shake manifest, D-AOT2)

The `RuntimeFn` enum is closed; `lower` can mint imports from this table
only, and `verify` (S1) fails on anything else. `RuntimeFn → (module_atom,
function_atom, arity)` is ONE static table; **the emitted ImpT chunk is
exactly the used subset, in first-use order** (IR-24). Generated code
imports no native/NIF module — `aion_flow_ffi` is reached only through
`aion_flow` (plan recon 7), so beamr's capability policy on native imports
is satisfied trivially.

| Module (mangled atom) | Functions/arity |
|---|---|
| `aion@workflow` | `define/5, run/1, all/1, map/2, spawn/6, spawn_and_wait/6, receive/1, with_timeout/2, sleep/1` |
| `aion@activity` | `new/5, task_queue/2, retry/2, timeout/2, node/2` |
| `aion@awl@error` | `codec/0, map_activity_error/1, map_receive_error/1, map_child_error/1, map_spawn_error/1, map_timer_error/1` |
| `aion@awl@codec` | leaf `{bool,int,float,string,nil}×{_codec/0,_to_json/1,_decoder/0}`, `nil_codec/0, raw/0, decoded/3, json_value/0` |
| `aion@awl@runtime` | `run/4, index/3` |
| `aion@codec` | `json_codec/2` |
| `aion@duration` | `milliseconds/1` |
| `aion@error` | `terminal/1` |
| `aion@signal` | `new/2` |
| `aion@child` | `await/1` |
| `gleam@json` | `object/1, string/1, array/2, nullable/2, to_string/1` |
| `gleam@dynamic@decode` | `field/3, optional_field/4, success/1, failure/2, then/2, map/2, list/1, optional/1`, `string` (representation per R3) |
| `gleam@list` | `flatten/1, filter/2, map/2, sort/2, length/1, try_fold/3, reverse/1, is_empty/1` |
| `gleam@option` | `is_some/1, is_none/1` (values `none`/`{some,V}` are terms, not calls) |
| `gleam@int` / `gleam@float` / `gleam@string` / `gleam@bool` | `compare/2` each |
| `gleam@string` | `append/2` (R2 primary) |
| **fallback rows** (marked; unused unless a register entry flips) | `gleam@result:try/2` (R1 fallback ONLY) |
| `erlang` (bif-position only) | `'+'/2` (the `Increment` burst's `gc_bif2` target), comparison ops via `gc_bif`/test instructions. **BC-2b-3 correction:** beamr's `Bif` instruction resolves its BIF through the import table, so `erlang:'+'/2` DOES occupy an ImpT row — exactly as OTP `.beam` files carry `gc_bif` targets. It remains bif-position only: `lower` never mints it as a `CallRt`/`TailRt` callee (`verify` rejects that alongside `ResultTry`), and it resolves to beamr's native pure `add`. |

Retry/backoff config constructs SDK records (`RetryPolicy`,
`Fixed`/`Exponential` — `stmts.rs:141-156`): `RecordNew` term shapes, not
imports; exact atoms pinned by contract row IR-10 against compiled
`aion_flow`.

**Notably absent — can never appear** (S15): no process primitives, no
receive loops, no `spawn`-family BIFs, no arithmetic beyond the loop-counter
`'+'` and guard comparisons, no `gleam@result` (R1 folds it away; retained
only as the marked fallback row), no `aion_flow_ffi` or any NIF module, no
`erlang` ImpT entries beyond the bif-position `'+'/2` row above, no dynamic
`apply/3`.

**Derived durable-operation summary** (S16): the golden printer and an
exported per-module summary classify used `RuntimeFn`s into
`timers | activities | children | signals` — the park-point-granularity
capability manifest for the beamr AOT track, layered above this flat table.

---

## 7. The IR contract table (D-AOT2)

Representation rows — each asserted by a BC-4 fixture test:

| # | Gleam value / construct | Erlang term / binding statement | Grounding |
|---|---|---|---|
| IR-1 | `String` | UTF-8 binary | draft §4 |
| IR-2 | `Int` / `Float` | integer / float; **float literal bytes must equal the reference's parse of the same source lexeme** (S3) | draft §4; `exprs.rs:151` |
| IR-3 | `Bool` | `true` / `false` atoms | draft §4 |
| IR-4 | `List(a)` | proper list | draft §4 |
| IR-5 | `Option(a)` | `{some, V}` / `none` | draft §4 |
| IR-6 | `Result(a, e)` | `{ok, V}` / `{error, E}` | draft §4; capstone B |
| IR-7 | custom record | `{snake_tag_atom, F1, …, Fn}`; zero-field ⇒ bare atom | draft §4; codec design §4.1 |
| IR-8 | enum variant | bare atom (Gleam constructor snake) | codec design §2 |
| IR-9 | `fn(…) -> …` | fun (`make_fun2/3` + FunT) or export fun; funs minted ONLY at the §2.6 sites | draft §4 |
| IR-10 | SDK constructor ABI | pinned tag atoms + arities used literally: `retry_policy`, `fixed`, `exponential`, `some/none`, `timed_out_error`, `inner_error`, `timeout_engine_failure`, all `AwlError` variants — pinned by test against compiled `aion_flow` | `stmts.rs:141-156, 296-309`; codec design §6 |
| IR-11 | `Codec(a)` | record whose `encode`/`decode` fields are funs (T-ACTRAW's `call_fun` depends on this row; pinned by test) | `wrappers.rs:121-138` |
| IR-12 | module reference | mangled atom (`aion@workflow`, `gleam@dynamic@decode`, …) | draft §4; capstone imports |
| IR-13 | entry ABI | `run/1` receives the raw input payload term; returns `{ok, ResultBinary}` — bytes recorded verbatim as `WorkflowCompleted.result`; error path `{error, AwlErrorTerm}` | capstone obs. 8 |
| IR-14 | calling convention | args `x0..x(n-1)`, result `x0`; y-registers live across calls under `allocate`/`deallocate`/`trim`; routes/loop recursion are tail calls | draft §4; capstone B; validator recon 4 |
| IR-15 | export set | exactly `definition/0`, `run/1`, `execute/1`; no `module_info/0,1` | decision 12; capstone obs. 4 |
| IR-16 | error propagation | `result.try` sites lower structurally as flattened `TryBind` (§2.2); trail-invariant; instruction streams intentionally differ from erlc's; fallback R1 | capstone B |
| IR-17 | durations | constructed only via `aion@duration:milliseconds/1` from precomputed ms; no duration wire codec exists or can be constructed | codec design §2; `exprs.rs:23-34` |
| IR-18 | capability closure | the `RuntimeFn` enum (§6) is the complete import surface; anything outside it is a `verify` failure (S1) | §6 |
| IR-19 | sidecar | `.gleam_types` = `project_sidecar(&MirModule)`; **every** function has a descriptor signature (no omission); deterministic bytes; golden per fixture | §5; S2; X1 |
| IR-20 | rename compatibility | emitted structures restricted to what erlc output uses (plain import tables, `LitT` tuples/atoms/binaries, FunT lambdas) so `register_module_with_renames` applies unchanged | plan recon 9; capstone obs. 6, Deliverable A (44/44) |
| IR-21 | output-codec fallback | when the workflow has no success outcome, T-DEF/T-RUN reference `awlc.nil_codec` as output codec (`CodecRef::SdkNil`) | `frame.rs:157-163` (S7) |
| IR-22 | closure sidecar arity | lifted-closure sidecar rows record physical BEAM arity (declared params + appended captures); source arity recoverable from the `Fn` descriptor at the `MakeClosure` site | S9 |
| IR-23 | descriptor module spellings | `Custom` module-string vocabulary pinned against the `gleam-types` extractor's output for compiled `aion_flow` before goldens freeze | S10 |
| IR-24 | import-table determinism | ImpT = exactly the used `RuntimeFn` subset, in first-use order | S18 |

Writer-contract rows (evidence-grounded; the assembler's obligations):

| Contract | Statement | Grounding |
|---|---|---|
| chunk set/order | `AtU8, Code, ImpT, ExpT, FunT, LitT, StrT, Line` in the canonical order of `encode/container.rs:64-88`; empty `Line`/`StrT`/`FunT` legal | capstone obs. 5 |
| header counts | derived from the instruction stream, never hand-set | plan risk table |
| no `int_code_end` | decoder stops at end-of-bytes; writer emits no terminator — beamr-loadable by proof, NOT OTP-loadable; resurfaces at BC-5 only if artifacts are ever advertised OTP-loadable | decision 12; capstone obs. 3 |
| no `module_info/0,1` | not required by load/validate/execute or the catalog path | decision 12; capstone obs. 4 |
| Line chunk | emitted from MIR spans, file 0 = the `.awl` source name — runtime stacktraces anchor to author lines; `StrT` empty | R4 |
| rename bounds | emitted structures stay inside what the rename machinery handles: plain import tables, `LitT` tuples/atoms/binaries, FunT lambdas (`unique_id` recompute proven) | capstone obs. 6 |

---

## 8. Decision register (adjudicable deviations, with pre-authorized fallbacks — S12)

| # | Decision | Chosen | Pre-authorized fallback | Status |
|---|---|---|---|---|
| R1 | `result.try` lowering | flattened `TryBind` (§2.2, capstone-proven, trail-invariant under D-BC3) | re-point one op's recipe at `gleam@result:try/2` + continuation closure; MIR unchanged; fallback ImpT row already marked in §6 | ratified |
| R2 | `Concat` (`<>`) | `call_ext gleam@string:append/2` | erlc-style bs-op binary construction — only if the round-trip corpus proves the family beamr-supported for our shape (X4) | ratified; revisit only with corpus evidence |
| R3 | `decode.string` Decoder-constant materialization (zero-arity call vs export-fun literal) | **pinned by disassembling the reference-compiled corpus during BC-3 — never guessed** (all three designs converged on this; ratified once here) | n/a — this IS the pin action | open until BC-3 golden authoring |
| R4 | Line chunk from MIR spans | emit (spans exist on every op/AST node; loader accepts absence) | cut without ceremony if it drags BC-3 | ratified, droppable |

---

## 9. Determinism, goldens, verify

- `lower` is a pure function of the CheckedDocument: var numbering, atom
  interning, literal-pool ordering, capture ordering (liveness-ordered,
  deterministic), and function ordering derive from document order +
  `BTreeMap`/`BTreeSet` iteration (the `Plan` discipline). Same `.awl` ⇒
  same MirModule ⇒ same sidecar bytes (#218 dissolves at the MIR boundary).
- **MIR golden per fixture** (BC-2 acceptance ratchet #1): canonical text
  dump — first section = the effect schedule (S11: durable `CallRt`s in
  order with wire-visible arguments), then per-function bodies with
  `live_after` annotations (S14) and `degraded_parallel` markers (S13);
  expanded trio functions print in full (S8) with their
  `FnOrigin::CodecTemplate` provenance headers.
- **Sidecar golden per fixture** (ratchet #2): hex of `project_sidecar`.
- **`verify(&MirModule)` runs inside every golden test** (S1).
- BC-3 adds `validate_module`-over-every-fixture and per-shape unit tests
  keyed to the §2.5 op table, one test per row.

## 10. What BC-3 `select` consumes

`select` receives a verified `MirModule` and owns exactly: shell-template
expansion from the §2.4 fixed recipes; a single walk of each FlowFn body
emitting each op's burst from the §2.5 table; register allocation — args in
`x0..x(n-1)`, one linear-scan pass mapping the S14 `live_after` vars to
y-slots, frame size = peak y-count, `allocate`/`deallocate` bracketing per
validation layer 5; label resolution; literal pooling (LitT/AtU8); FunT
`unique_id` assignment; ImpT construction per IR-24; assembly through
`beamr::loader::encode::encode_module` (beamr **0.14.0 from crates.io,
feature `encode`** — the operator amendment). There is no instruction
*selection* problem left and no function synthesis left (S8): every choice
was made in this document, and the only optimization-shaped pass (regalloc)
arrives with its spill contract precomputed.

---

## Appendix A — type sketch (ratified; amendments applied)

```rust
// RATIFIED MIR — crate-private to aion-awl (decision 3). No registers, no
// labels: BC-3 owns both. Single-assignment Vars. Files split per house
// rules: mir/{module,template,flow,ops,lower,verify,sidecar,print}.rs,
// mod.rs re-exports only.

// ---------- identity ----------
pub(crate) struct Var(u32);                 // single-def, per-function
pub(crate) struct AtomRef(u32);
pub(crate) struct LitRef(u32);
pub(crate) struct FnRef(u32);
pub(crate) struct Span { pub line: u32, pub column: u32 } // -> Line chunk (R4)

// ---------- module ----------
pub(crate) struct MirModule {
    pub name: String,
    pub source: String,                     // .awl file name (Line chunk file 0)
    pub atoms: Vec<String>,
    pub literals: Vec<MirLiteral>,
    pub exports: Vec<FnRef>,                // exactly run/1, definition/0, execute/1
    pub functions: Vec<MirFn>,
    pub types: Vec<TypeShape>,
    // NO sidecar field — the sidecar is project_sidecar(&MirModule) (S2).
}

pub(crate) enum MirLiteral {
    Integer(i64),
    Float { lexeme: String },               // S3: source lexeme retained
    Atom(AtomRef), Binary(Vec<u8>),
    Tuple(Vec<MirLiteral>), Nil, List(Vec<MirLiteral>),
}

pub(crate) enum TyDesc {                    // total sidecar projection source (S2)
    Bool, Int, Float, String, Nil,
    List(Box<TyDesc>), Option(Box<TyDesc>),
    Result(Box<TyDesc>, Box<TyDesc>),
    Tuple(Vec<TyDesc>),
    Custom { module: String, name: String, params: Vec<TyDesc> }, // params! (S2)
    Fn(Vec<TyDesc>, Box<TyDesc>),
    Dynamic, Json, AwlError,
    Decoder(Box<TyDesc>), Codec(Box<TyDesc>),
    Activity(Box<TyDesc>, Box<TyDesc>),
    SignalRef(Box<TyDesc>),
    WorkflowDefinition(Box<TyDesc>, Box<TyDesc>, Box<TyDesc>),
    Duration,
    Unknown,                                // empty-list provenance; projects as List(Nil)
                                            // (gleam_type renders bare Nil, types.rs:116 — §5)
}

pub(crate) enum TypeShape {
    Record { name: String, tag: AtomRef, fields: Vec<FieldShape> },
    Enum   { name: String, variants: Vec<(AtomRef, String)> },
    Union  { name: String, arms: Vec<UnionArm> },
}
pub(crate) struct FieldShape { pub awl_name: String, pub desc: WireDesc, pub optional: bool }
pub(crate) struct UnionArm { pub outcome: String, pub ctor: AtomRef, pub payload: WireDesc }

/// AWL-BC-CODEC-DESIGN §2 Desc as pure lowering-time data (decision 9).
pub(crate) enum WireDesc {
    Bool, Int, Float, Str, Nil,
    List(Box<WireDesc>), Nullable(Box<WireDesc>),
    Ref(String),
}

// ---------- functions ----------
pub(crate) enum MirFn {
    Templated(TemplateFn),                  // SHELLS ONLY post-S8
    Flow(FlowFn),
}

pub(crate) enum TemplateFn {
    Definition { workflow_name: String, input_codec: FnRef, output_codec: CodecRef },
    Run        { input_codec: FnRef, output_codec: CodecRef },
    Execute    { input_fields: Vec<(String, TyDesc)>, entry: FnRef, entry_args: Vec<u16> },
    ActivityWrapper { action: String, input: TypeShapeRef, params: Vec<TyDesc>,
                      input_codec: FnRef, return_codec: CodecRef },
    ActivityWrapperRaw { action: String, input: TypeShapeRef, params: Vec<TyDesc>,
                         input_codec: FnRef },
    SignalRef  { signal: String, payload_codec: CodecRef },
    DeadBody,
    ChildWitness,
    // CodecTrio REMOVED: trios expand at lower time into FlowFns (S8).
}
pub(crate) struct TypeShapeRef(u16);
pub(crate) enum CodecRef { Local(FnRef), SdkNil /* IR-21 */, SdkLeaf(Leaf) }
pub(crate) enum Leaf { Bool, Int, Float, Str, Nil }

pub(crate) enum FnOrigin {                  // S6 provenance, on every function
    Run, Definition, Execute,
    Region { entry_step: String },
    SubStep { parent: String, sub: String },
    Loop { step: String, index: u32 },
    ActivityWrapper { action: String, raw: bool },
    SignalRef { signal: String },
    DeadBody, ChildWitness,
    CodecTemplate { kind: CodecTemplateKind, subject: String, params: TrioParams },
    LiftedClosure { host: FnRef, index: u32 },
}
pub(crate) enum CodecTemplateKind { RecordTrio, EnumTrio, UnionTrio, CompositeTrio }
/// Descriptor-style template parameters retained for a post-BC descriptor
/// revisit (S8 / continuation-first D6): field specs, arm specs, inner desc.
pub(crate) enum TrioParams {
    Record { shape: TypeShapeRef },
    Enum { shape: TypeShapeRef },
    Union { shape: TypeShapeRef },          // zero is OP-BUILT in the body (S5)
    Composite { desc: WireDesc },
}

pub(crate) struct FlowFn {
    pub origin: FnOrigin,
    pub name: String,                       // step_x / sub_x_y / x_loop_k / awl_fun_k / <stem>_codec ...
    pub params: Vec<Var>,                   // Plan liveness params (or args+captures for Lifted)
    pub param_tys: Vec<TyDesc>,             // physical arity for Lifted (S9 / IR-22)
    pub ret_ty: TyDesc,
    pub body: Block,
    pub span: Span,
    pub degraded_parallel: bool,            // S13 marker (printed in goldens)
}

pub(crate) struct Block { pub stmts: Vec<Stmt>, pub tail: Tail }

// ---------- values ----------
pub(crate) enum Value {
    Var(Var), Lit(LitRef), Atom(AtomRef), Int(i64), Nil,
}

// ---------- statement ops (closed; one instruction burst each) ----------
pub(crate) struct LiveAfter(pub Vec<Var>);  // S14: printed in goldens

pub(crate) enum Stmt {
    Bind        { dst: Var, value: Value, span: Span },
    FieldGet    { dst: Var, base: Value, index: u16, span: Span },
    RecordNew   { dst: Var, tag: AtomRef, args: Vec<Value>, span: Span },
    ListNew     { dst: Var, items: Vec<Value>, span: Span },
    CallRt      { dst: Option<Var>, callee: RuntimeFn, args: Vec<Value>,
                  live_after: LiveAfter, span: Span },
    CallLocal   { dst: Option<Var>, callee: FnRef, args: Vec<Value>,
                  live_after: LiveAfter, span: Span },
    CallClosure { dst: Option<Var>, fun: Value, args: Vec<Value>,
                  live_after: LiveAfter, span: Span },
    MakeClosure { dst: Var, lifted: FnRef, captures: Vec<Value>, span: Span },
    TryBind     { dst: Var, result: Var, live_after: LiveAfter, span: Span }, // §2.2 / R1
    WaitTimeoutCase { dst: Var, receive: FnRef, captures: Vec<Value>,
                      deadline_ms: u64, span: Span },
    Cmp         { dst: Var, op: CmpOp, lhs: Value, rhs: Value, span: Span },
    BoolOp      { dst: Var, op: BoolBin, lhs: Value, rhs: Value, span: Span }, // S4
    Not         { dst: Var, src: Value, span: Span },                          // S4
    Concat      { dst: Var, lhs: Value, rhs: Value, span: Span },              // R2
    Increment   { dst: Var, src: Var, span: Span },
    AssertList  { binds: Vec<Option<Var>>, list: Var, span: Span },
    AssertSome  { dst: Var, option: Var, span: Span },
    JsonObj     { dst: Var, pairs: Vec<(String, JsonVal)>, span: Span },
    IndexGuard  { dst: Var, base: Var, index: u64, message: String, span: Span },
    Attempt     { lifted: FnRef, captures: Vec<Value>, defs: Vec<Var>,
                  on_ok: Block, on_err: Block, span: Span },
    // If / SelectEnum are NOT statements — promoted to Tail (S17).
}

pub(crate) enum JsonVal { Encoded { value: Value, via: ToJsonRef } }
pub(crate) enum ToJsonRef { SdkLeaf(Leaf), Local(FnRef) }

pub(crate) enum Test {                      // test-position; short-circuit = nested If tails
    IsTrue(Value),
    Cmp { op: CmpOp, lhs: Value, rhs: Value },
    IsTagged { value: Value, tag: AtomRef, arity: u16 },
    Not(Box<Test>),
}
pub(crate) enum CmpOp { Eq, Ne, Lt, Le, Gt, Ge, FLt, FLe, FGt, FGe }
pub(crate) enum BoolBin { And, Or }

// ---------- tails (S17: control constructs end blocks, by type) ----------
pub(crate) enum Tail {
    Return(Value),
    TailLocal { callee: FnRef, args: Vec<Value> },     // call_last / call_only
    TailRt    { callee: RuntimeFn, args: Vec<Value> }, // call_ext_last (shells)
    If        { test: Test, then_block: Box<Block>, else_block: Box<Block>, span: Span },
    SelectEnum { subject: Value, arms: Vec<(AtomRef, Block)>, span: Span },
}

// ---------- the closed import surface (§6; = tree-shake manifest) ----------
pub(crate) enum RuntimeFn {
    // aion@workflow
    WfDefine, WfRun, WfAll, WfMap, WfSpawn, WfSpawnAndWait, WfReceive, WfWithTimeout, WfSleep,
    // aion@activity
    ActNew, ActTaskQueue, ActRetry, ActTimeout, ActNode,
    // aion@awl@error
    ErrCodec, MapActivityError, MapReceiveError, MapChildError, MapSpawnError, MapTimerError,
    // aion@awl@codec
    LeafToJson(Leaf), LeafDecoder(Leaf), NilCodec, RawCodec, Decoded, JsonValueCodec,
    // aion@awl@runtime
    RtRun, RtIndex,
    // aion@codec / aion@duration / aion@error / aion@signal / aion@child
    JsonCodec, DurationMs, ErrorTerminal, SignalNew, ChildAwait,
    // gleam@json
    JObject, JString, JArray, JNullable, JToString,
    // gleam@dynamic@decode
    DField, DOptionalField, DSuccess, DFailure, DThen, DMap, DList, DOptional, DString,
    // gleam@list
    LFlatten, LFilter, LMap, LSort, LLength, LTryFold, LReverse, LIsEmpty,
    // gleam@option / compares / string
    OIsSome, OIsNone, CmpInt, CmpFloat, CmpString, CmpBool, StrAppend,
    // R1 fallback ONLY (unused in the primary design; marked row in §6):
    ResultTry,
}

// ---------- entry points ----------
// lower: total for checked documents; Err == a D-BC3 refusal (span-anchored).
// pub(crate) fn lower(input: &CheckedDocument<'_>) -> Result<MirModule, EmitError>;
// verify: S1 — capability closure, arity, single-def, tail invariants,
//         TypeEnv cross-checks at spans; runs under every golden.
// pub(crate) fn verify(module: &MirModule) -> Result<(), VerifyError>;
// project_sidecar: S2 — the D-AOT1 artifact is a fold over `functions`.
// pub(crate) fn project_sidecar(module: &MirModule) -> Vec<u8>;
// print_mir: §9 golden format — effect schedule first (S11), live_after (S14),
//            degraded_parallel (S13), expanded trios in full (S8).
// pub(crate) fn print_mir(module: &MirModule) -> String;
```

---

## 11. BC-3 — selection and register allocation (design of record, 2026-07-12)

BC-3 consumes a verified `MirModule` per §10 and owns everything from there to
`.beam` bytes: shell expansion, instruction selection from the §2.5 burst
table, register allocation, label resolution, chunk construction, and assembly
through `beamr::loader::encode::encode_module` (**beamr 0.14.0 from crates.io,
feature `encode`** — the operator amendment; no path-dep anywhere, D-BC4
satisfied vacuously). Decisions 1–13 honored; decision 12 applies (no
`int_code_end`, no `module_info/0,1` — the shipped encoder already emits no
terminator). Binding constraints for this section, per the operator
(2026-07-12): emit only structures erlc output uses at the chunk/term level
(IR-20), and **no JIT-visible Y-relative access** — the X-registers-only rule,
decoded against the verified register-file reality in §11.1.

### 11.1 The register-file reality (code-verified; the docs are conceptual)

`beamr/docs` describes the process stack conceptually (`docs/files/`
`04-processes.md`: "its stack (where it is in its work)") and the typed-JIT
direction (`docs/AOT-NORTH-STAR.md`: the JIT consumes `.gleam_types` sidecars,
`TypedRegisterState`); the register-file specifics live in code. Verified
2026-07-12, cited by file:line against the beamr checkout:

1. **Interpreter** — per-process flat X file; Y registers are per-frame slots.
   `allocate`/`allocate_heap`/`allocate_zero` push a Y-frame
   (`interpreter/opcodes/core.rs:245-270`, `push_y_frame` 790-805);
   `deallocate` pops it (807-811); frames isolate their Y slots
   (`process/stack.rs:39-47`, test `y_registers_are_isolated_by_frame`
   stack.rs:384). Crucially, **return points are pushed by `call`/body
   `call_ext` themselves** (`core.rs:101-124` pushes a 0-slot frame,
   `core.rs:165-178` `ExtCallReturn::Body`) — unlike OTP, `allocate` saves no
   CP, so **a frameless non-tail call is legal in this VM**.
2. **Validator** — one linear pass; a Y operand is legal only under a live
   `Allocate…Deallocate` bracket with index < frame size
   (`loader/validate.rs:33-48, 98-115, 211-229`); `Deallocate` resets the
   tracked frame to `None` (validate.rs:229) — layout consequence in §11.3.
   X register indices must be < 256 (validate.rs:94).
3. **JIT** — the compile unit is ONE function (`jit/compile_job.rs:13-33`).
   `Register::Y(i)` flat-maps to register-file slot `X_REGISTER_COUNT + i`
   (= 1024+i) — **not frame-relative** (`jit/ir_common.rs:12-22, 161-167`).
   The `Allocate`/`Deallocate`/`Trim` family has **no lowering anywhere in
   `jit/`** (grep-verified): any function containing one fails compilation
   (`UnsupportedOpcode` fallthrough, `jit/compiler/dispatch_data.rs:333`) and
   runs interpreted forever. `CallExt` compiles to a helper that re-enters the
   interpreter (`jit/compiler/dispatch_call.rs:47-89`).
4. **GC** — gc points carrying a `Live` operand treat only `x0..x(live-1)` as
   roots and **clear X registers above `live`** (`interpreter/opcodes/`
   `core.rs:326-337` re `clear_dead_x_regs`/`gc::minor`;
   `guards.rs:174-180`). A callee's own gc points therefore wipe the caller's
   high X registers: **X can never carry a value across any call, in either
   engine.** `put_list`/`put_tuple2` self-reserve defensively with the full
   register file rooted (core.rs:320-345).

**Theorem (why a global no-Y rule is impossible for this MIR).** A value live
across a non-tail call must survive callee X-clobber and callee GC clearing
(fact 4). The only callee-save storage the machine offers is the frame Y slot;
the §6 capability set (S15) forbids every escape hatch (no process dictionary,
no ETS, no erlang call surface), and rewriting call-crossing live ranges into
SDK-driven continuation closures contradicts §2.2 (flattened `TryBind` exists
precisely to avoid that), S8 (`select` never synthesizes functions), and
D-BC1 parity. Region bodies with sequential `TryBind` chains have non-empty
`live_after` by construction. Strict X-only-everywhere is therefore not a
BC-3 design option — it would be a different MIR.

**Resolution — the two-tier rule (R5).** The X-only constraint's operative
content is that the JIT's broken flat-Y mapping must be unreachable for our
output. BC-3 guarantees that outright:

- **Tier 1 (frameless, X-only, JIT-eligible):** functions whose crossing set
  (§11.2) is empty emit **no `Allocate`, no Y operand, ever** — arguments and
  temporaries in X, frameless body calls per fact 1, tails via
  `CallOnly`/`CallExtOnly`.
- **Tier 2 (framed, interpreter-pinned by construction):** functions with a
  non-empty crossing set open with `Allocate F` and home call-crossing values
  in Y. Because the JIT compiles per function and has no `Allocate` lowering
  (fact 3), **no function containing a Y operand is ever JIT-compiled** — the
  flat mapping is dead code for BC output. Y never appears without `Allocate`
  (the validator enforces it), so there is zero JIT-visible Y-relative access.

Framed functions are exactly the park-bound workflow glue (regions, shells
holding codec results); their JIT-ineligibility costs nothing today and lifts
automatically when the ABI brief gives the JIT frame-relative Y.

### 11.2 Register allocation

**Call-bearing ops** (their bursts contain an X-clobbering call): `CallRt`,
`CallLocal`, `CallClosure`, `Concat` (call_ext `gleam@string:append/2`, R2),
`IndexGuard` (call_ext `aion@awl@runtime:index/3` + TryBind burst), `JsonObj`
(per-pair to_json calls), `WaitTimeoutCase` (call_ext `duration:milliseconds`
+ `with_timeout/2`), `Attempt` (`call_fun`); `TryBind` only under the R1
fallback (its primary burst is test+extract, call-free). **Not call-bearing:**
`MakeClosure` (heap op; captures consumed at the op, defensive full-file
rooting per fact 4's put-op pattern), `Increment` (gc_bif with an accurate
`Live`), `Bind`/`FieldGet`/`RecordNew`/`ListNew`/`Cmp`/`BoolOp`/`Not`/
`AssertList`/`AssertSome` (pure or heap-only bursts).

**Liveness: recomputed, not trusted.** `select` recomputes full backward
liveness per function (same discipline as `lower/liveness.rs`) because the
S14 `live_after` annotations cover only `CallRt`/`CallLocal`/`CallClosure`/
`TryBind` — the five fused call-bearing ops carry none (defect D1, §11.7).
The S14 sets become a cross-check: any mismatch on the covered ops is a hard
`EmitError`, never silently patched.

**Crossing set** `X(f)` = union of live-across sets over all call-bearing ops
in the function's block tree, plus one internal accumulator temp per
`JsonObj` with ≥ 2 pairs (its pair list accumulates across its own interior
calls). Params used after any call-bearing op are members by construction.

**Tier assignment** is the per-function predicate `X(f) = ∅` (no class
promises; it falls where it falls). Expected tier-1 population: T-EXEC
(pure prelude + `call_only`), T-SIG (one codec call with nothing live, then
`call_ext_last`), T-DEAD, T-WIT, enum `_to_json` (select + arm moves +
`call_ext_last json:string/1`), the enum-decoder `is_eq_exact` cascade,
comparators/accessors, keel-only helpers. Expected tier-2: regions, loops,
substeps, T-DEF/T-RUN/T-ACT/T-ACTRAW (2+ codec-call results held
simultaneously), record `_to_json` with ≥ 2 encoded fields, decoder
continuations, `<stem>_codec/0` (the S8 closure is live across the decoder
call; `select` does NOT reorder MIR to dodge this — determinism and parity
outrank a 1-slot frame).

**Homes.** Tier-2: members of `X(f)` get Y slots in first-definition order
(params in `X(f)` are spilled `move x_i → y_j` in the prologue, immediately
after `Allocate F`; thereafter ALL uses reload from Y). Everything else lives
in X. Frame size `F = |X(f)| +` internal temps; **no Y-slot reuse in v1**
(single-def vars; frames are small — bounded by the emitter's liveness-param
discipline) and **no `Trim` ever** (R6).

**X discipline.** Segments = maximal call-free runs. After every call-bearing
op, every X register is dead except `x0` (live-across values are in Y by
construction), so X allocation is per-segment fresh numbering — no intervals,
no graph coloring, no spill search. Y slots are touched **only by `move`**
(reload y→x before a use-run, store x→y right after a def) — every other
instruction sees X/literal/atom operands only, keeping burst templates
uniform and trivially validator-clean. Call marshaling is a standard
parallel-move into `x0..x(k-1)` with one scratch X above the segment
high-water for cycles; `CallFun` puts the fun in `x(arity)`; `MakeFun`
captures go in `x0..x(free-1)` (convention confirmed by
`jit/compiler/dispatch_call.rs` `make_fun_free_var_operands`).

**Live-operand accuracy (hard rule).** Every emitted `Live` operand
(`TestHeap`, `GcBif2`, `AllocateHeap`) = current live-X high-water + 1,
because GC clears X above `Live` (fact 4). One coalesced `TestHeap` per
heap-allocating run (tuple arity+1 words, 2 per cons); beamr's put ops
self-reserve defensively, but we emit accurate reservations anyway (erlc
parity, no reliance on VM slack).

**No-spill argument.** There is no spill machinery to get wrong: the
"allocation" is a deterministic partition (crossing → Y homes, rest →
per-segment fresh X). X pressure per segment ≤ params + defs-in-segment +
reloads + marshal width, all statically bounded by MIR shape (widest:
`RecordNew`/`ListNew`/call marshals, ≤ max record/args arity). The emitter
asserts X < 256 and arity ≤ 255 at emit time (validator caps,
validate.rs:94) — an `EmitError`, never silent.

### 11.3 Layout, labels, exits (the one-pass-validator discipline)

- **Function header:** `Label(k)` / `FuncInfo` / `Label(k+1)` (the erlc
  two-label shape from the round-trip corpus); tier-2 then `Allocate F` +
  prologue spills.
- **Body:** blocks in MIR tree order; `If` arms and `SelectEnum` arms emitted
  then/else and declaration order; deferred blocks (badmatch targets for
  `AssertList`/`AssertSome`/enum-total `SelectVal` fail — `Badmatch`,
  unreachable by construction, emitted valid) after the body.
- **Single shared exit (R7).** validate.rs is a linear scan and `Deallocate`
  clears its frame tracking (fact 2) — a mid-stream `Deallocate; Return` would
  invalidate every later Y operand in the same function. Therefore each framed
  function has **exactly one** `Deallocate`, linearly last: every
  `Return(value)` tail moves its value to `x0` and jumps to the shared
  `Lexit: Deallocate F; Return`; `TryBind` fail branches jump straight to
  `Lexit` (`x0` already holds `{error, E}` — §2.2 semantics unchanged);
  tail calls leave via `CallLast{deallocate: F}` / `CallExtLast{deallocate:
  F}`, which the validator's frame tracking ignores (validate.rs:211-229
  handles only the Allocate family and standalone `Deallocate`). Tier-1
  returns inline (`move → x0; Return`), tails via `CallOnly`/`CallExtOnly`.
- **Labels** are symbolic during emission and numbered sequentially
  module-wide at finalize; `label_count`/`function_count`/`opcode_max` are
  derived from the stream by the encoder (writer contract: header counts
  never hand-set).

### 11.4 Per-node instruction templates

Value operands: `Var` → home register (Y homes via reload move), `Lit` →
`Operand::Literal(pool index)`, `Atom` → `Operand::Atom(Some(_))`, `Int` →
`Operand::Integer`, `Nil` → `Operand::Atom(None)`. All variant names are
beamr `loader::decode::Instruction` / `Operand` types (the encode feature
shares them — no format knowledge duplicated).

| MIR node | Burst (conventions of §11.2 apply) |
|---|---|
| `Bind` | `Move` value → home |
| `FieldGet` | `GetTupleElement { source, element: index, dest }` — the MIR `index` is ALREADY the element index (`lower` emits `position+1`); the earlier "`index+1`" wording assumed a 0-based MIR ordinal and was wrong against the shipped BC-2 lowering (D3b, corrected in the BC-3 commit) |
| `RecordNew` | zero-field: `Move` bare tag atom; else `TestHeap`(coalesced) + `PutTuple2 { dest, elements: [tag, args…] }` |
| `ListNew` | fully-constant: `Move` of pooled `LitT` list; else `TestHeap` + `PutList` chain from nil |
| `CallRt` | marshal → `CallExt { arity, import }`; result `x0` → home |
| `CallLocal` | marshal → `Call { arity, label }`; result `x0` → home |
| `CallClosure` | marshal args, fun → `x(arity)` → `CallFun { arity }`; result `x0` → home |
| `MakeClosure` | captures → `x0..f-1` → `MakeFun` (lambda index); result `x0` → home |
| `TryBind` | `IsTaggedTuple { fail: Lexit-or-Lfail, value: x0, arity: 2, tag: 'ok' }` + `GetTupleElement x0[1]` → home (§2.2, capstone Deliverable B shape) |
| `WaitTimeoutCase` | captures → `MakeFun`; `Move deadline_ms` → `CallExt duration:milliseconds/1`; marshal → `CallExt with_timeout/2`; nested `IsTaggedTuple` cascade over the 4 arms building `{ok,{some,V}}`/`{ok,none}`/error terms (`PutTuple2`), arms `Jump` to a local continuation label; result → home |
| `Cmp` | `Comparison { op, fail: Lfalse, lhs, rhs }` + `Move 'true'` + `Jump Ldone`; `Lfalse: Move 'false'`; `Ldone:` (ComparisonOp Lt/Ge/Eq/Ne/EqExact/NeExact; Int/Float split is MIR-level semantics, same test instructions) |
| `BoolOp` / `Not` | test burst (`Comparison EqExact` vs `'false'` for `Not`) + true/false materialization as `Cmp` |
| `Concat` | marshal → `CallExt gleam@string:append/2` (R2); result → home |
| `Increment` | `Bif { op: GcBif2, operands: [fail 0, Live: high-water+1, import(erlang:'+'/2), src, 1, dst] }` — see A1 |
| `AssertList` | `GetList`/`GetHd`/`GetTl` chain + `TypeTest IsNil` on the final tail, fail → badmatch block |
| `AssertSome` | `IsTaggedTuple { tag: 'some', arity: 2, fail: badmatch }` + `GetTupleElement` |
| `JsonObj` | per pair: reload value, to_json call (`CallExt` leaf / `Call` local), `PutTuple2` pair (name binary from `LitT`), cons onto accumulator (Y-homed when ≥2 pairs); then `CallExt gleam@json:object/1` |
| `IndexGuard` | marshal (base, index, message binary) → `CallExt aion@awl@runtime:index/3` + TryBind burst |
| `Attempt` | captures → `MakeFun`; fun → `x0`… `CallFun { arity: 0 }`; `IsTaggedTuple {ok,2}` → `GetTupleElement` defs-tuple destructure → homes → on_ok block; fail edge → on_err block (compensation ends in a route tail — refusal preserved) |
| `Tail::Return` | `Move → x0` + `Jump Lexit` (tier-2) / `Return` (tier-1) |
| `Tail::TailLocal` | marshal → `CallLast { label, deallocate: F }` (tier-2) / `CallOnly` (tier-1) |
| `Tail::TailRt` | marshal → `CallExtLast { import, deallocate: F }` / `CallExtOnly` |
| `Tail::If` | `Comparison`/`TypeTest`/`IsTaggedTuple` per `Test` (Not = inverted op; short-circuit already nested by MIR) fail → else-label; then-block; else-block |
| `Tail::SelectEnum` | `SelectVal { value, fail: badmatch, list: [atom→label…] }`; arm blocks in declaration order |

**Shell expansion (one selector).** At entry, `select` expands each §2.4
`TemplateFn` into a FlowFn-shaped body **in the §2.5 op set** (T-DEF:
`MakeClosure(execute)` + name-binary `Bind` + codec `CallLocal`s/`CallRt`s +
`TailRt WfDefine`; T-RUN, T-EXEC, T-ACT, T-ACTRAW (`CallClosure` on the codec
record's `encode` field per IR-11), T-SIG, T-DEAD, T-WIT likewise, verbatim
from the §2.4 recipe column). Recipes stay name-substitution-only and mint at
most one closure (S8 intact); after expansion, ONE selection engine owns
every function — there is no second instruction-emission path for shells.

### 11.5 Assembly pipeline

```
verified MirModule
  → expand      (shells → §2.5-op FlowFn bodies; fixed recipes)
  → classify    (liveness recompute + S14 cross-check; crossing sets;
                 tier; Y homes; frame sizes; X high-water)
  → emit        (per-function burst walk of §11.4; symbolic labels;
                 §11.3 layout; emit-time caps: X<256, arity≤255)
  → finalize    (label numbering; atoms interned via beamr AtomTable;
                 ImpT in first-use order = used RuntimeFn subset ∪
                 bif-position rows (A1, IR-24 as amended);
                 ExpT = exactly run/1, definition/0, execute/1 labels
                 (decision 12); FunT in MakeClosure first-use order
                 (unique_id is loader-derived — decode/chunks.rs:28-38 —
                 and rename-recomputed, capstone obs. 6); LitT deduped
                 first-use (MirLiteral → decode::chunks::Literal; float
                 bytes from the S3 lexeme parse); Line instructions +
                 line_info rows from op spans (R4, A2))
  → beamr::loader::encode::encode_module(&parsed, &atom_table)   (0.14.0, "encode")
  → self-gate   (load_beam_chunks → resolve_imports → validate_module
                 on every emit — production path and tests; a rejection
                 is an EmitError, never a silent artifact)
```

Determinism: every ordering above is a pure function of the `MirModule`
(first-use, first-definition, tree order) — same `.awl` ⇒ same MIR ⇒ same
bytes; #218 holds through BC-3. BC-3 also performs the deferred S1
`TypeEnv` per-op result-type cross-check (it holds the environment during
selection, per the BC-2 status note) and executes the **R3 pin** before
goldens freeze: disassemble the reference-compiled corpus for the
`decode.string` Decoder-constant materialization — never guessed.

BC-3's ratchets (plan §3 row): `validate_module` over every checking fixture,
plus one per-shape unit test per §11.4 row.

### 11.6 Contract amendments (D-AOT2 maintenance rule — same-commit deltas)

- **A1 (§6 / IR-24).** `erlang:'+'/2` **does occupy an ImpT row** when
  `Increment` is used: gc_bif resolves its target through
  `module.resolved_imports` (beamr `interpreter/opcodes/guards.rs:182-186`).
  §6's "`erlang` (bif-position only, never ImpT)" is corrected to
  "bif-position only — present in ImpT when used, never a `call_ext` target";
  IR-24 becomes "ImpT = used `RuntimeFn` subset ∪ used bif-position entries,
  in first-use order". (Deliverable A's 44-module corpus loads such rows
  today; capability policy unaffected — `erlang` is not a native/NIF module.)
- **A2 (§7 Line row).** The shipped encoder writes `num_fnames = 0`
  (`loader/encode/chunks.rs:114-134`) — there is no filename table. The
  "file 0 = the `.awl` source name" clause narrows to: line numbers anchor to
  `.awl` source lines (`LineInfo { file: 0, line }` from op spans, bound via
  `Instruction::Line { index }`); filename association is by module↔source
  convention. R4's droppability is unchanged.
- **A3 (IR-14 narrowed).** "y-registers live across calls under
  allocate/deallocate/trim" applies to tier-2 functions only; tier-1
  functions make frameless non-tail calls (legal per §11.1 fact 1) and use no
  Y at all; `trim` is never emitted (R6).
- **New decision-register rows (§8):**

| # | Decision | Chosen | Pre-authorized fallback | Status |
|---|---|---|---|---|
| R5 | register policy | two-tier X/Y (§11.1): tier-1 frameless X-only (JIT-eligible), tier-2 framed Y homes (JIT-refused by construction — zero JIT-visible Y access) | all functions framed with `Allocate 0+F` bracketing (erlc-idiom parity; cost: everything interpreter-pinned) if any engine surprise with frameless body calls | ratified this section |
| R6 | `Trim` | never emitted (frames die at the single `Deallocate`) | emit trims if frame growth ever measurably matters (it cannot at these sizes) | ratified |
| R7 | exit layout | single shared `Lexit: Deallocate F; Return` per framed function, linearly last (one-pass validator discipline, §11.3) | per-exit `Deallocate` with all Return-exits sorted last in linear order | ratified |
| R8 | tier-2 Y-homing granularity + tier predicate (BC-3 v1) | **conservative: every var (params + all defs) is homed in Y, not only crossing-set members** — so Y is touched ONLY by `move` (reload before use, store after def), no X carries a value across any op, and TestHeap/GcBif `Live` = the exact per-burst X high-water. **Tier predicate = `frame_size > 0` (BC-3 v1 ships R5's pre-authorized fallback, below): any function with a parameter or a defined var is framed; only a param-and-def-free body is frameless.** This supersedes the crossing-set predicate for v1 — a var-bearing function with an empty crossing set (`execute/1`, T-EXEC, T-SIG, T-DEAD, comparators) is framed/interpreter-pinned, not frameless tier-1. Trades a few extra Y slots + JIT-eligibility (frames stay small, `< 256`; JIT-ineligibility costs nothing today per §11.1) for a uniform, provably validator-clean burst emitter with a single emission path | **crossing-set tier-1 (R5-primary): frameless when the crossing set is empty, vars in per-segment-fresh X (§11.2), JIT-eligible** — plus per-segment-X minimization (Y homes only for crossing-set members). An additive BC-3 refinement; the frame layout is internal, so tightening it changes no ABI | fallback taken this commit (BC-3 build); revisit with the JIT ABI brief |

### 11.7 Defects exposed by BC-3 planning, and dependencies

- **D1 (advisory, non-structural).** S14 `live_after` coverage misses the
  five fused call-bearing ops (`Concat`, `IndexGuard`, `JsonObj`,
  `WaitTimeoutCase`, `Attempt`): their bursts contain X-clobbering calls but
  carry no annotation, so the golden-printed y-spill contract is incomplete.
  `select` recomputes liveness authoritatively (§11.2) and cross-checks S14
  where present. Recommended follow-up: extend the annotation (or at least
  the golden printer) to those ops so regalloc-relevant diffs stay visible in
  goldens — an additive MIR increment, not a blocker.
- **D2 (doc defect — fixed by A1).** §6's "never ImpT" claim for `erlang`
  was false against beamr's bif resolution path.
- **D3 (doc narrowing — fixed by A2).** The Line-chunk filename clause was
  unimplementable with the shipped encoder.
- **Dependencies into BC-3:** the pending BC-2 increments (T-DEAD/T-ACTRAW/
  T-WIT shells; record/enum/union `_decoder` bodies + enum/union `_to_json`;
  composite trios; D4 optional-field `_to_json`) must land before their §11.4
  rows can be golden'd — BC-3 proceeds on the covered subset in the same
  increment order, and the `decode.success(nil)` placeholders emit as
  ordinary (visibly wrong, structurally valid) bursts in the interim, never
  silently correct.
- **No structural MIR defect found.** Every §2.5 op has a bounded burst under
  the two-tier rule; no op needs reordering, interior register-pressure
  decisions, new MIR nodes, or select-time function synthesis. The one true
  conflict — IR-14's Y-spill contract vs the X-only constraint — is resolved
  by R5 with zero JIT-visible Y access, grounded in §11.1 facts 1–4.

### 11.8 BC-3 implementation status (2026-07-13)

The `crates/aion-awl/src/mir/select` module (private submodule of `mir`;
`select(&MirModule) -> Result<Vec<u8>, SelectError>`) ships the selection +
register-allocation + assembly pipeline of §11.5 against beamr `0.14.0`
(feature `encode`) from crates.io.

**Landed and oracle-pinned** (the BC-3 oracle: every emitted module re-loads
and passes `validate_module` through all five loader layers — `load_beam_chunks`
→ `resolve_imports` → `validate_module`, exactly the capstone's standalone
path, run inside `select` on every emit so a rejection is a hard `SelectError`,
never a silent artifact):

- **Shell expansion** T-DEF, T-RUN, T-EXEC, T-ACT (§2.4 recipes; T-ACT mints
  the shared T-DEAD dead-body lambda + its `FunT` entry, so `activity:new/5`'s
  fifth argument and the FunT population are real).
- **Ops** `FieldGet`, `RecordNew` (tuple + zero-field bare-atom), `CallRt`
  (`call_ext`), `CallLocal` (`call`), `MakeClosure` (`make_fun2` + FunT),
  `TryBind` (flattened §2.2), `JsonObj` (incl. the ≥2-pair Y-homed accumulator),
  `ListNew`, `Cmp`, `BoolOp`, `Not`, `AssertSome` (checked `{some, payload}`
  extraction; explicit `Badmatch` failure), `TupleNew` (untagged
  `put_tuple2`), `Increment` (`gc_bif2 erlang:'+'` against a real ImpT row;
  fail label 0 is deliberate — non-integer raises `badarith` like Gleam's
  `+`). **Tails** `Return`, `TailRt`
  (`call_ext_last`/`call_ext_only`), `TailLocal` (`call_last`/`call_only` —
  loop self-recursion is a framed `CallLast`, so iteration never grows the
  stack), `If`, `SelectEnum` (explicit `CaseEnd` mismatch trap).
- **Pools** deterministic atom table, literal pool (first-use dedup, `MirLiteral
  → decode::chunks::Literal`, S3 float lexeme parse), import table (used
  `RuntimeFn` subset in first-use order, IR-24), `FunT` (`MakeClosure`/execute/
  dead-body first-use). `ExpT` = exactly `run/1`, `definition/0`, `execute/1`
  (decision 12, no `module_info`). Chunk set/order owned by `encode_module`.
- **The register allocator** ships R5's pre-authorized fallback (§11.6 R8 as
  amended this commit): all-Y homing for every function that has a parameter or
  a defined var (framed, `Allocate F`), and frameless only for a body with
  neither (a tail over immediates). The tier predicate is therefore `frame_size
  > 0`, NOT the crossing set — a var-bearing function with an empty crossing set
  (e.g. `execute/1`) is framed/interpreter-pinned. Crossing-set tier-1
  (frameless with vars in X, JIT-eligible) is the deferred R5-primary refinement
  (§11.6 R8 fallback column); it costs nothing today (§11.1: the JIT consumes
  sidecars, a post-BC path). Determinism: the whole pipeline is a pure function
  of the `MirModule` (`select` twice ⇒ identical bytes — #218 holds through
  BC-3).
- **Oracle coverage**: all 30 `valid/` fixtures that BC-2 lowers emit +
  validate (loop evidence: `loop_counting_until_max`,
  `backward_route_bounded_cycle`, `loop_after_fall_through`,
  `loop_compound_until_nested`, and `loop_without_counting`); the 26 fixtures
  BC-2 refuses stay refused (no MIR ⇒ nothing to emit — never a silent skip).
  Plus per-shape unit tests for the
  reached §11.4 rows, including explicit nonzero failure-label checks for
  outcome guards, checked `AssertSome` + `Badmatch`, an explicit `CaseEnd`
  trap check for enum-total dispatch, and the `gc_bif2 erlang:'+'/2`
  increment burst check.

**Honest D-BC3 refusals (`SelectError::Unsupported`, span-anchored — the
not-yet-reachable §11.4 rows; none appears in a BC-2-lowered fixture, so this
narrows nothing the oracle covers):** shells T-ACTRAW / T-SIG / T-WIT; ops
`Bind`, `CallClosure`, `WaitTimeoutCase`, `Concat`, `AssertList`,
`IndexGuard`, `Attempt`. Each lands with its §11.4 burst when the BC-2 increment
that constructs it (decoder bodies, composite trios, remaining keel
expressions) lands — BC-3
proceeds on the covered subset in the same increment order (§11.7 D-BC3
dependency note), never emitting a shape it cannot verify.

**R4 / A2 (Line chunk):** not emitted in v1 — `line_info` is left empty (the
loader treats an absent `Line` chunk as empty; R4 is explicitly droppable and
A2 records the shipped encoder writes no filename table). Re-add from op spans
if runtime stacktraces are wanted; it changes no ABI.
