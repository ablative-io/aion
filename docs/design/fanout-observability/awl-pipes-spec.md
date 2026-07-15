# SPEC — Make PIPES compile on the AWL direct-to-BEAM path

Lane: `aion-awl` direct compiler (`crates/aion-awl/src/mir/lower` + `src/mir/select`). Reference semantics = the Gleam emitter (`crates/aion-awl/src/emitter/*`). Baseline verified live on main: **BC-3 oracle 46 of 69 corpus shapes emitted, 23 refused** (reproduced by lowering every `tests/fixtures/rev2/**/valid/*.awl`).

---

## 1. Pipe-form inventory (what the language has, what the reference does, what the direct path does today)

### 1.1 Surface forms

- `Statement::Pipe(PipeStmt)` — `head |> stage |> … -> name` or `… |> route target` (crates/aion-awl/src/ast/steps.rs:51, PipeStmt at ast/steps.rs:182-195). Stages: `PipeStage::Action{name}` (one-argument action **or child** stage), `PipeStage::Field{name}` (`.field` projection), `PipeStage::Combinator(CombinatorCall)` (ast/steps.rs:124-141). Combinator vocabulary: `Filter/Map/Sort/Count/Any/All` (ast/steps.rs:145-158), arg is an optional `Expr` (accessor for filter/map/sort, full element predicate for any/all). Terminators: `PipeEnd::Bind(Binding)` / `PipeEnd::Route(RouteTarget)` (ast/steps.rs:173-178).
- Parser: statement pipes parse in `parse_pipe_statement` (parser/statements.rs:192-237); stages in `parse_pipe_stage` (parser/statements.rs:239-303). **Inventory subtlety:** a statement-head `xs |> any(p)`/`xs |> all(p)` is absorbed by expression postfix parsing into `Expr::CollectionPredicate` (parser/exprs.rs:140-167) — those already lower on the direct path (mir/lower/expr.rs:118-125 → mir/lower/collection_predicate.rs:45-119, execution-parity proven in tests/runtime_codecs.rs:200-300). `PipeStage::Combinator(Any|All)` therefore only occurs **after a prior stage** (e.g. `xs |> filter(.f) |> any(p)`).
- Checker: `walk_pipe` (checker/stages.rs:19-49). Action stages must name a declared action **or child** with exactly one parameter (stages.rs:53-88); filter/map/sort take a `.field` accessor (stages.rs:172-191, filter's field must be Bool per stages.rs:120-135); count takes no arg (stages.rs:114-118); any/all take an element predicate (stages.rs:145-155). NOTE: the checker does **not** constrain sort's key comparability — that gate lives only in the emitter (emitter/pipes.rs:139-147), so a check-clean document can reach lowering with a non-comparable sort key.

### 1.2 Reference lowering (the parity contract)

`emitter/pipes.rs`:
- `lower_pipe_value` (emitter/pipes.rs:181-218): head rendered, each stage binds a fresh `awl_piped_{position}`, threading value+type; terminator belongs to the caller (`emitter/steps.rs:451-467` — Bind inserts scope binding; Route passes the piped value as payload).
- Action stage → single-param action: `<name>_activity(arg) |> [retry] |> [timeout] |> task_queue |> [node] |> workflow.run |> awl_error.map_activity_error` in a `use … result.try` (emitter/pipes.rs:230-259); optional-wrap of the piped value via `wrap_optional` (emitter/pipes.rs:295-308).
- Action stage → single-param **child**: `workflow.spawn_and_wait(string_lit(name), CHILD_WITNESS, json.object([#(param_name, to_json(wrapped_arg))]), awlc.json_value(), <child_output_codec>(), awl_error.codec()) |> awl_error.map_child_error` (emitter/pipes.rs:261-285). Neither action nor child → error "`{name}` names neither a declared action nor a child workflow" (emitter/pipes.rs:287-290). Multi-arg action/child in a pipe → error (emitter/pipes.rs:231-239, 262-270; checker also rejects, stages.rs:70-80).
- Field stage → `let fresh = current.field` (emitter/pipes.rs:197-199).
- Combinators (`render_combinator`, emitter/pipes.rs:82-177):
  - `filter` → `list.filter(current, fn(item) { item.field })` (100-106)
  - `map` → `list.map(current, fn(item) { item.field })` (107-110)
  - `count` → `list.length(current)` (111)
  - `sort` → `list.sort(current, fn(left, right) { <cmp>(left.field, right.field) })` with `<cmp>` = `int.compare`/`float.compare`/`string.compare`/`bool.compare` chosen by the resolved key type; anything else is a hard refusal "`sort` needs a comparable key (Int, Float, String, Bool), found …" (112-153)
  - `any`/`all` → `render_predicate_over` (154-176 → emitter/collection_predicates.rs:36-79): infallible predicate → `list.any/list.all(collection, fn(item){pred})`; fallible predicate (contains `workflow.id` or indexing — `is_fallible`, collection_predicates.rs:81-106) → `list.try_fold` with decisive short-circuit.
  - Stage result types (`stage_type`, emitter/pipes.rs:20-79): count→Int, filter|sort→incoming, map→List(projected field), any|all→Bool, action/child→declared return, field→field type.

### 1.3 Direct path today — refusal sites

- `PipeStage::Action` and `PipeStage::Field` and both `PipeEnd`s **already lower** (mir/lower/flow.rs:261-273 for the ends; flow.rs:316-360 `lower_pipe_value`; activity stage rides `activity_call` with the `piped` argument — mir/lower/activity.rs:39-143, optional-wrap parity at mir/lower/expr.rs:290-312).
- **Refusal A — `pipe combinator`**: flow.rs:354-356 (`PipeStage::Combinator` → `LowerError::unsupported("pipe combinator", …)`). This is the census-moving gap: it refuses 6 valid fixtures today (see §5).
- **Refusal B — pipe-into-child**: an action stage naming a child reaches `activity_value`'s "child call or unknown action" (mir/lower/activity.rs:48-53) because `lower_pipe_value` unconditionally builds a `Call` and calls `activity_call` (flow.rs:327-341). The checker accepts child pipe stages and the emitter lowers them, so this is a genuine direct-path gap (no rev2 fixture exercises it; census does not move, but the lane contract is 1:1 form coverage with execution parity).
- **Kept refusals (do NOT change)**: "multi-arg action in pipe" (activity.rs:58-64 — checker-unreachable, emitter also errors, keep as defensive parity); i64-overflow guards ("integer literal above i64::MAX", mir/lower/expr.rs:47-48; "retry count above i64::MAX", activity.rs:255-256); plain `Statement::Call` child calls stay refused (flow.rs:293-313 + activity.rs:48-53 — out of this lane, see open questions).

All needed runtime imports already exist in the closed capability table: `LFilter/LMap/LSort/LLength/LAny/LAll/LTryFold` and `CmpInt/CmpFloat/CmpString/CmpBool` (mir/runtime.rs:88-105, signatures 176-192); `verify` only rejects `ResultTry`/`IntAdd` (mir/verify.rs:222-236). `WfSpawnAndWait/MapChildError/ChildAwait/ErrCodec/JsonValueCodec` likewise exist (runtime.rs:32,47,62-67).

---

## 2. Implementation design

### 2.1 New file `src/mir/lower/pipes.rs` — pipe-stage dispatch + combinator lowering

Move from flow.rs, verbatim (behavior-neutral):
- `lower_pipe_value` (flow.rs:316-360) — becomes `pub(super) fn lower_pipe_value(ctx, plan, head, stages, scope, stmts) -> Result<(Value, GType), LowerError>`.
- the local free fn `field_index` (flow.rs:362-382) — keep its exact messages ("`.{field}` needs a record", "no field `{field}`"); do NOT swap it for `Ctx::field_index` (mir/lower/expr.rs:396-414), whose message differs ("needs a record type") — no error-text drift.

Rework the `PipeStage::Action` arm (currently flow.rs:327-341) to mirror the reference's action-then-child resolution order (emitter/pipes.rs:230/261/287):

```rust
PipeStage::Action { name, span } => {
    if ctx.emitter.actions.contains_key(name.as_str()) {
        // unchanged: build Call{args: vec![]}, activity_call(ctx, plan, &call, Some((value, ty)), scope, stmts)?
        // ty = actions[name].returns via type_ref_to_g (as today, flow.rs:335-339)
    } else if ctx.emitter.children.contains_key(name.as_str()) {
        let (bound, returns) = super::child_call::pipe_child_stage(ctx, plan, name, *span, (value, ty), stmts)?;
        value = Value::Var(bound);
        ty = returns;
    } else {
        return Err(LowerError::new(*span,
            format!("`{name}` names neither a declared action nor a child workflow"))); // reference message, emitter/pipes.rs:287-290
    }
}
```

Replace the `PipeStage::Combinator` refusal (flow.rs:354-356) with `lower_combinator_stage`:

```rust
fn lower_combinator_stage(ctx, current: Value, current_ty: &GType, combinator: &CombinatorCall,
                          scope: &Scope, stmts: &mut Vec<Stmt>) -> Result<(Value, GType), LowerError>
```

1. Element type: `let GType::List(elem) = ctx.emitter.env.resolve(current_ty) else { return Err(LowerError::new(combinator.span, "this combinator needs a list")) }` (mirror of emitter/pipes.rs:40-51; checker-unreachable).
2. Accessor extraction for filter/map/sort: `Some(Expr::Accessor{name, span})` else `LowerError::new(combinator.span, "this combinator takes a `.field` accessor in the Gleam stopgap")` (mirror emitter/pipes.rs:91-99).
3. Per kind, mirroring `render_combinator` (emitter/pipes.rs:100-176) and `stage_type` (emitter/pipes.rs:52-76):
   - **Count**: `call_rt(ctx, RuntimeFn::LLength, vec![current], stmts, span)` → `(Var, GType::Int)`. Ignore any arg (checker rejects it; emitter ignores it).
   - **Filter**: projection closure over `elem`'s field (see below), then `call_rt(LFilter, vec![current, Value::Var(closure)])` → `(Var, current_ty.clone())`.
   - **Map**: same projection closure; `call_rt(LMap, …)` → `(Var, GType::List(Box::new(field_ty)))`.
   - **Sort**: resolve key `field_ty` via `field_index(ctx, &elem, name, span)`; pick `RuntimeFn::CmpInt/CmpFloat/CmpString/CmpBool` by `ctx.emitter.env.resolve(&field_ty)` being `Int/Float/Str/Bool`; **any other key type refuses** `LowerError::unsupported("`sort` over a non-comparable key (needs Int, Float, String, Bool)", combinator.span)` — this is the reference's hard gate (emitter/pipes.rs:139-147) mapped to the direct path's `Unsupported` class (precedent: forks.rs:23-25 "everything the reference refuses, we refuse (clean Unsupported)"; `Unsupported` keeps the BC-3 oracle green if a corpus shape ever hits it, select/tests.rs:75). Then compare closure (below) + `call_rt(LSort, vec![current, Value::Var(closure)])` → `(Var, current_ty.clone())`.
   - **Any/All**: `let predicate = combinator.arg.as_ref().ok_or_else(|| LowerError::new(combinator.span, "collection predicate needs an argument"))?` (mirror emitter/pipes.rs:164-166); quantifier = `Any→Quantifier::Any, All→Quantifier::All` (mirror emitter/pipes.rs:167-171); delegate to the extracted `lower_predicate_over` (§2.2) with the already-lowered `current` value and `elem` — returns `(Value, GType::Bool)` and handles fallible/infallible + captures identically to the proven `Expr::CollectionPredicate` path.

**Projection closure** (filter/map — the direct twin of `fn(item) { item.field }`):
- `let (index, field_ty) = field_index(ctx, &elem, accessor_name, accessor_span)?;`
- `let (ordinal, reference) = ctx.take_predicate()?;` — combinator closures ride the existing dynamically-allocated lifted-closure slot machinery (mir/lower/ctx.rs:88-104; slots appended after all fixed helpers, driver.rs:108-110, base at build.rs:282-291), so **no slot pre-counting is needed**.
- Build under a fresh var namespace (`ctx.swap_var_counter(0)` … restore — same protocol as collection_predicate.rs:66-81):
  - params `[item]`, `param_tys [ctx.tydesc(&elem)]`, `ret_ty ctx.tydesc(&field_ty)`
  - body: `Stmt::FieldGet { dst, base: Value::Var(item), index, span }`; `Tail::Return(Value::Var(dst))`
  - `name: format!("awl_combinator_{ordinal}")`, `origin: FnOrigin::LiftedClosure { host: FnRef(2), index: ordinal as u32 }` (same origin scheme as collection_predicate.rs:173-176), `degraded_parallel: false`.
- `ctx.finish_predicate(ordinal, MirFn::Flow(f));` then in the host: `Stmt::MakeClosure { dst, lifted: reference, captures: vec![], span }`.

**Compare closure** (sort):
- params `[left, right]`, `param_tys [elem_td, elem_td]`, `ret_ty TyDesc::Custom { module: "gleam/order".into(), name: "Order".into(), params: vec![] }` (TyDesc::Custom is the SDK-nominal escape hatch, mir/tydesc.rs:44-50; sidecar maps it generically, mir/sidecar.rs:54-66).
- body: two `FieldGet`s (left.field, right.field), `Tail::TailRt { callee: <cmp>, args: vec![Value::Var(lf), Value::Var(rf)] }` (frameless tail import is a selected shape — select/tests.rs:479-502).
- Same take/finish + var-namespace protocol and naming as the projection closure.

### 2.2 `src/mir/lower/collection_predicate.rs` — extract `lower_predicate_over`

Split `lower_collection_predicate` (collection_predicate.rs:45-119) at the point where the collection is already a `Value` (after lines 54-57):

```rust
pub(super) fn lower_predicate_over(ctx, items: Value, element: &GType, quantifier: Quantifier,
    predicate: &Expr, span: crate::Span, scope: &Scope, stmts: &mut Vec<Stmt>)
    -> Result<(Value, GType), LowerError>
```

containing lines 58-118 verbatim (captures = predicate refs ∩ scope; take_predicate; `build_predicate_fn`; MakeClosure with host captures; `LTryFold` w/ decisive-initial atom when `predicate_is_fallible`, else `LAny`/`LAll`; `TryBind` on the fallible result). `lower_collection_predicate` becomes: lower the collection expr, resolve the list element (keep its exact error, lines 54-57), delegate. This is the direct twin of the emitter's `render_collection_predicate`/`render_predicate_over` split (emitter/collection_predicates.rs:11-34 / 36-79). **Refactor must be statement-for-statement identical for existing callers** — the committed MIR goldens for `collection_predicates`, `fallible_*` fixtures must not change (§4 step 3).

### 2.3 New file `src/mir/lower/child_call.rs` — shared child spawn machinery + the pipe child stage

Move from fork_child.rs, verbatim: `to_json_ref` (fork_child.rs:480-486), `codec_value` (fork_child.rs:488-511). Add:

```rust
pub(super) fn spawn_wait_args(ctx, plan: &FnPlan, name: &str, span: crate::Span,
    returns: &GType, input: Var, stmts: &mut Vec<Stmt>) -> Result<Vec<Value>, LowerError>
```

— the tail of `child_spawn_args` (fork_child.rs:443-477) generalized: witness closure over `plan.child_witness` (Planning error "…has no planned witness" if absent), `input_codec = call_rt(JsonValueCodec)`, `output_codec = codec_value(child_output_codec_ref_for(ctx, plan, returns)?)` (build.rs:489-496; the envelope codec is registered for **every declared child** — registry.rs:98-117 — so no registry change is needed), `error_codec = call_rt(ErrCodec)`, `name_lit = ctx.binary(name)`; returns `vec![Lit(name), Var(witness), Var(input), Var(input_codec), Var(output_codec), Var(error_codec)]`. Update `fork_child::child_spawn_args` to keep building its multi-param `JsonObj` pairs (fork_child.rs:414-442) and then call `spawn_wait_args` — **the emitted statement order must be unchanged** (input JsonObj, witness, input codec, output codec, error codec) so the `child_collection_fork*` goldens stay byte-identical.

```rust
pub(super) fn pipe_child_stage(ctx, plan, name: &str, span: crate::Span,
    piped: (Value, GType), stmts) -> Result<(Var, GType), LowerError>
```

1:1 with the emitter's child pipe stage (emitter/pipes.rs:261-285):
1. `let child = ctx.emitter.children.get(name)` (caller guarantees present); `returns = type_ref_to_g(&child.returns)`.
2. `let [param] = child.params.as_slice() else { return Err(LowerError::unsupported("multi-arg child in pipe", span)) }` (checker-unreachable; mirrors emitter/pipes.rs:262-270).
3. `expected = type_ref_to_g(&param.ty)`; `wrapped = wrap_optional_value(ctx, value, &value_ty, &expected, stmts, span)` (mir/lower/expr.rs:290-312 — same helper the piped-action path uses; matches emitter/pipes.rs:273).
4. Single-pair input: `Stmt::JsonObj { dst: input, pairs: vec![(param.name.clone(), JsonVal::Encoded { value: wrapped, via: to_json_ref(ctx, plan, &expected)? })], span }` (ops.rs:207-210; encodes with the **declared param type**, matching emitter/pipes.rs:272-277).
5. `let args = spawn_wait_args(…)?;` `waited = call_rt(WfSpawnAndWait, args)`; `mapped = call_rt(MapChildError, vec![Var(waited)])`; `Stmt::TryBind { dst: bound, result: mapped, … }` → `Ok((bound, returns))` — the exact spawn/map/try shape already execution-proven for fork children (fork_child.rs:262-277, tests/runtime_codecs/child_envelope.rs).

### 2.4 `src/mir/lower/forks.rs` — witness planning for pipe child stages

`needs_child_witness` (forks.rs:91-112) must detect child-naming pipe stages, **checking the pipe's own stages before honoring its route-end early stop** (today line 107 breaks before looking inside the pipe):

```rust
Statement::Pipe(pipe) => {
    if pipe.stages.iter().any(|stage| matches!(stage,
        crate::ast::PipeStage::Action { name, .. }
            if emitter.children.contains_key(name.as_str()))) {
        return true;
    }
    if matches!(pipe.end, crate::ast::PipeEnd::Route(_)) { break; }
}
```

The `Statement::Route(_) => break` and loop-body recursion (forks.rs:101-106) stay. `count_fork_fns`/`collect_raw_actions` are unaffected (combinator closures use the dynamic predicate slots, not fork slots). `fixed_helper_refs`/`predicate_start` adjust automatically (build.rs:219-231, 282-291). Pipes inside loop bodies get both features for free — loop bodies reuse `flow::lower_statement` (mir/lower/loops.rs:34, 345); collection-fork bodies cannot contain pipes (single-unbound-call gate, forks.rs:300-341) and named-fork branches with pipes already refuse (fork_named.rs:37-41).

### 2.5 Wiring + hygiene

- `src/mir/lower/mod.rs` (mod list at lines 3-25): add `mod child_call;` and `mod pipes;`.
- `src/mir/lower/flow.rs`: delete the moved fns; `Statement::Pipe` arm (flow.rs:261-273) calls `pipes::lower_pipe_value`; update the module doc (flow.rs:1-13) — combinators are no longer in the deferred list; pipes with combinator stages and child stages are covered. Chain-boundary liveness needs **no change**: pipe heads/combinator args/ends are already collected (mir/lower/chain.rs:81-99).
- File-size law check: flow.rs 525→~458 lines; fork_child.rs 522→~465; new pipes.rs ~330 and child_call.rs ~150 — all ≤500 code lines. mod.rs stays re-exports/mod-decls only.
- No `unwrap/expect/panic`, no `#[allow]`, backticked identifiers in all doc comments (doc_markdown is DENY).

---

## 3. Test plan

### 3.1 MIR pins (in-crate)

New `src/mir/pipe_tests.rs`, registered `#[cfg(test)] mod pipe_tests;` in `src/mir/mod.rs` (alongside mod.rs:21-30). Use the `lower_source`/`print_mir` style of deferred_tests.rs:18-21 and tests.rs:229-249:

1. `combinator_stages_lower_to_list_runtime_calls` — lower `tests/fixtures/rev2/step-bodies/valid/combinators.awl`; `verify(&module)?`; `print_mir` must contain: `gleam@list:filter/2`, `gleam@list:sort/2`, `gleam@list:map/2`, `gleam@list:length/1`, `gleam@int:compare/2`, `make_closure`, and an `awl_combinator_` function name.
2. `sort_string_key_selects_string_compare` — inline source sorting by a `String` field; print contains `gleam@string:compare/2`.
3. `post_stage_any_all_lower_through_predicate_closures` — inline source with `xs |> filter(.flag) |> any(.n >= 3)` and an `all` twin; print contains `gleam@list:any/2`, `gleam@list:all/2`, and `awl_predicate_` functions (proves `PipeStage::Combinator(Any|All)` routes through `lower_predicate_over`).
4. `pipe_child_stage_lowers_to_spawn_and_wait` — inline pipe-into-child source (shape of §3.3's `pipe_child.awl`); print contains `aion@workflow:spawn_and_wait/6`, `aion@awl@error:map_child_error/1`, and the module's function list includes `awl$child_witness` (proves §2.4's witness planning).

`src/mir/deferred_tests.rs`: **replace** `post_join_combinator_refuses_with_its_focused_class` (deferred_tests.rs:63-95 — its document now lowers) with:
- `post_join_combinator_now_lowers` — same source, assert `lower_source(source)??` succeeds and `verify` passes (keeps the historical anchor as a coverage pin);
- `sort_non_comparable_key_refuses_with_the_reference_class` — check-clean source `docs |> sort(.inner) |> count -> total` where `.inner` is a record field (the checker does not gate comparability, stages.rs:137-139); assert `Err(LowerError::Unsupported { shape, .. })` with `shape == "`sort` over a non-comparable key (needs Int, Float, String, Bool)"`.

### 3.2 Golden ratchet (census-moving fixtures)

`src/mir/tests.rs` COVERED (tests.rs:67-114) gains exactly these five entries (keep list sorted as-is by section):
- `dag-fork/valid/fork_collection_join`
- `dag-fork/valid/fork_sequential`
- `schema-doors/valid/import_nested_defs`
- `schema-doors/valid/mixed_doors`
- `step-bodies/valid/combinators`

Then bless goldens: `AWL_BC2_BLESS=1 cargo test -p aion-awl lowers_cover_and_golden`, commit the ten new files (`.mir` + `.gleam_types.hex` per fixture under `tests/mir-goldens/…`), re-run WITHOUT bless. **Refactor-neutrality gate:** after blessing, `git status` must show only the ten new golden files — any *modified* existing golden (especially `child_collection_fork*`, `collection_predicates`, `fallible_*`, `pipe_chain_stages`) means the §2.2/§2.3 refactors were not statement-neutral; fix the refactor, do not bless the drift.

`step-bodies/valid/step_bodies_combined` stays refused — its refusal advances from `pipe combinator` (line 24) to `wait` (its `hold_for_ack` step); no pin exists on it, no action needed.

### 3.3 Execution-parity tests (MANDATORY quality bar)

New module `tests/runtime_codecs/pipes.rs`, registered in `tests/runtime_codecs.rs` next to the existing `#[path]` mods (runtime_codecs.rs:20-30): `#[path = "runtime_codecs/pipes.rs"] mod pipes;`. Parity documents live in a NEW directory `crates/aion-awl/tests/fixtures/parity/` — deliberately OUTSIDE `tests/fixtures/rev2` so the BC-3 oracle census, the COVERED ratchet, and the parser/checker corpus scans (all keyed to rev2, e.g. select/tests.rs:23-29, tests.rs:20-26) keep a fixed 69-fixture denominator.

**Fixture `tests/fixtures/parity/pipe_combinators.awl`** (entry step is pure — reachable without any activity dispatch):

```awl
//! Pipe-combinator parity: filter, sort (Int and String keys), map, count,
//! and post-stage any/all over one findings list.
workflow pipe_combinators
  input findings: [Finding]
  outcome tallied: type Tally, route success

type Finding { title: String, blocking: Bool, severity: Int }
type Tally {
  blocker_titles: [String],
  alpha_titles: [String],
  blocker_count: Int,
  total: Int,
  any_severe: Bool,
  all_severe: Bool,
}

step tally
  findings |> filter(.blocking) -> blockers
  blockers |> sort(.severity) |> map(.title) -> blocker_titles
  blockers |> sort(.title) |> map(.title) -> alpha_titles
  blockers |> count -> blocker_count
  findings |> count -> total
  findings |> filter(.blocking) |> any(.severity >= 3) -> any_severe
  findings |> filter(.blocking) |> all(.severity >= 3) -> all_severe
  route tallied(blocker_titles: blocker_titles, alpha_titles: alpha_titles,
    blocker_count: blocker_count, total: total, any_severe: any_severe,
    all_severe: all_severe)
```

Test `pipe_combinators_execute_with_reference_parity` — clone the proven execute-driven pattern (tests/runtime_codecs.rs:229-254 + `production_execute_driver`, tests/runtime_codecs/drivers.rs:413-438):
- Direct side: read+parse+`lower` the parity path (local helper — `drivers::lowered` is rev2-relative, drivers.rs:217-222); push an `awl$rt_execute` driver: build four `finding` records (tag atom `finding`; field order title, blocking, severity) — `("gamma", true, 3), ("alpha", false, 1), ("delta", true, 2), ("beta", true, 2)` — a `Body::list`, the input record with tag atom `pipe_combinators_input` (naming convention proven at drivers.rs:414, emitter/context.rs:108), `call_local(FnRef(2), …)` (execute), return. Push a second driver `awl$rt_execute_empty` with `findings = []` (`Body::list(vec![])`). `select`, load into `build_vm`.
- Reference side (harness::reference_module_at over the same file, harness.rs:71-81, since `harness::fixture` is rev2-anchored):

```gleam
pub fn awl_rt_execute() {
  execute(PipeCombinatorsInput(findings: [
    Finding(title: "gamma", blocking: True, severity: 3),
    Finding(title: "alpha", blocking: False, severity: 1),
    Finding(title: "delta", blocking: True, severity: 2),
    Finding(title: "beta", blocking: True, severity: 2),
  ]))
}

pub fn awl_rt_execute_empty() {
  execute(PipeCombinatorsInput(findings: []))
}
```

- Assertions: `direct == reference` for both drivers (one VM, per runtime_codecs.rs:200-227 style); `direct.starts_with("{ok,")`; and a semantic ordering check that the result contains `delta` before `beta` before `gamma` in the severity-sorted titles (duplicate key 2/2 pins sort **stability** parity) and `beta` before `delta` in the alpha-sorted titles. Expected semantics: blockers = [gamma, delta, beta]; severity-sorted titles `["delta","beta","gamma"]`; alpha titles `["beta","delta","gamma"]`; blocker_count 3; total 4; any_severe true; all_severe false; empty run: counts 0, any false, all true (vacuous truth on both sides).

**Fixtures `tests/fixtures/parity/pipe_child.awl` + `tests/fixtures/parity/score_essay.awl`** (pipe-into-child, modeled on the proven cross-module child proof, tests/runtime_codecs/child_envelope.rs):

```awl
//! Pipe-into-child parity: thread one value through a child stage, then project.
workflow pipe_child
  input essay: String
  outcome noted: type Note, route success

type Grade { score: Int, feedback: String }
type Note { feedback: String }

child score_essay(essay: String) -> Grade

step delegate
  essay |> score_essay |> .feedback -> feedback
  route noted(feedback: feedback)
```

```awl
//! The child: score anything, echo the essay through the feedback line.
workflow score_essay
  input essay: String
  outcome scored: type Grade, route success

type Grade { score: Int, feedback: String }

step score
  route scored(score: 42, feedback: "scored: " + essay)
```

Test `pipe_child_stage_executes_with_reference_parity`:
- Direct: lower+select BOTH parity docs; on the parent push driver `awl$rt_run` = `call_local(FnRef(0), vec![Value::Lit(r#"{"essay":"hello"}"#)])` (run/1-driven, exactly like child_envelope.rs:65-77).
- Reference: `reference_module_at` for both docs; parent driver `pub fn awl_rt_run() { run(dynamic.string("{\"essay\":\"hello\"}")) }` (child_envelope.rs:22-25 pattern); `gleam_build(&[("ref_pipe_child", …), ("ref_score_essay", …)])`.
- Host FFI: a local `pipe_child_host_ebin(label, child_module) -> PathBuf` builder in pipes.rs modeled on `harness::child_host_ebin` (harness.rs:139-227) but minimal: writes+`erlc`s an `aion_flow_ffi.erl` with `spawn_child(<<"score_essay">>, Input, _Config) -> ChildId = Input, Result = case {child_module}:run(Input) of {ok, Output} -> {ok, <<"ok:", Output/binary>>}; {error, _} -> {error, <<"child run failed">>} end, erlang:put({awl_child_result, ChildId}, Result), {ok, ChildId}.` plus the matching `await_child/1` (copy harness.rs:160-168 verbatim — this "ok:"-prefixed protocol is the production envelope contract already proven there). Build under `scratch_build_dir` (harness.rs:53-60 — NEVER /tmp).
- Two VMs: direct VM = reference ebins (for the SDK packages) + host ebin (child_module `score_essay`) + both direct modules; call `vm.call0("pipe_child", "awl$rt_run")`. Reference VM = reference ebins + host ebin (child_module `ref_score_essay`); call `vm.call0("ref_pipe_child", "awl_rt_run")`.
- Assertions: `direct == reference`; `direct.starts_with("{ok,")`; `direct.contains("scored: hello")` (the child's output survived the strict outcome-envelope decode AND the post-child `.field` projection). If beamr's 233-word top-level test heap overflows on this driver, switch to the `aion_awl_test_heap` spawn-runner + `call0_large` exactly as harness.rs:189-211/child_envelope.rs does — do not shrink the assertion.

---

## 4. Ordered checklist for the implementer

1. Create the lane worktree under the repo (`git -C /Users/tom/Developer/ablative/aion worktree add /Users/tom/Developer/ablative/aion/.worktrees/<lane> -b <lane-branch> main`); all cargo commands with `CARGO_TARGET_DIR=/Users/tom/Developer/ablative/aion/target`; gate outputs to full files under `<worktree>/<lane>-gates/` with an exit-code manifest; never pipe cargo output through grep/head/tail.
2. **Refactors first, behavior-neutral, then prove neutrality**: (a) extract `lower_predicate_over` in collection_predicate.rs (§2.2); (b) create child_call.rs and re-point fork_child.rs (§2.3, order-preserving); (c) create pipes.rs, move `lower_pipe_value` + `field_index` out of flow.rs (§2.1 move only). Run `cargo test -p aion-awl` — the entire suite including `lowers_cover_and_golden` (NO bless) must pass with zero golden drift before any new behavior lands.
3. Land the combinator lowering (§2.1 `lower_combinator_stage` + closures) and the child pipe stage (§2.1 Action-arm rework + §2.3 `pipe_child_stage` + §2.4 witness detection).
4. Update `src/mir/deferred_tests.rs` (§3.1 replacement + sort ratchet); add `src/mir/pipe_tests.rs` + mod registration.
5. Update COVERED (+5), bless goldens, verify only ten new golden files appear (§3.2), re-run the golden test without bless.
6. Add `tests/fixtures/parity/` docs + `tests/runtime_codecs/pipes.rs` + mod registration; run the runtime parity tests (they need the `gleam` and `erlc` binaries on PATH, like the existing runtime_codecs suite).
7. Update the stale module docs: flow.rs:1-13 (combinators/pipe-child no longer deferred), forks.rs header if touched, pipes.rs/child_call.rs get proper `//!` docs.
8. `cargo fmt` (never a format check).
9. Full gates, each to its own log + manifest entry:
   - `cargo build -p aion-awl`
   - `cargo clippy --all-targets -p aion-awl`
   - `cargo test -p aion-awl` — then additionally run `cargo test -p aion-awl every_lowered_fixture_emits_and_validates -- --nocapture` and record the oracle line VERBATIM. Expected: `BC-3 oracle: 51 fixtures emitted + validated, 18 refused` (movement +5 emitted / −5 refused; the five fixtures are exactly the COVERED additions in §3.2; `step_bodies_combined` advances `pipe combinator`→`wait` with no census change). If the numbers differ, STOP and reconcile before committing — do not adjust the expectation to fit.
   - `cargo test -p aion-awl-package`
   - `cargo check -p aion-server`
10. Stage explicit paths only (the touched src files, test files, the ten goldens, the three parity fixtures); commit on the lane branch with the `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` trailer. No push, no merge, no main, no deploys.

## 5. Expected census movement (pre-verified by lowering every rev2 valid fixture on main)

Baseline 46 emitted / 23 refused. The six `pipe combinator` refusals and their fates:
- `dag-fork/valid/fork_collection_join.awl` (line 20) → **emits** (action fork + count/filter + `is empty` guard, all otherwise-covered)
- `dag-fork/valid/fork_sequential.awl` (line 20) → **emits** (sequential fork + count/filter|>count)
- `schema-doors/valid/import_nested_defs.awl` (line 16) → **emits** (schema-door record + `.lines |> count` + already-supported `order |> pack |> route`)
- `schema-doors/valid/mixed_doors.awl` (line 36) → **emits** (`.attachments |> count`, configured pipe action, optional `.contact` projection)
- `step-bodies/valid/combinators.awl` (line 18) → **emits** (filter/sort/map/count + payload routes)
- `step-bodies/valid/step_bodies_combined.awl` (line 24) → still refused, advances to `wait`

Net: **51 emitted / 18 refused of 69**. Pipe-into-child moves no census (no rev2 fixture uses it) — its proof is the §3.3 parity test plus the §3.1 MIR pin.
