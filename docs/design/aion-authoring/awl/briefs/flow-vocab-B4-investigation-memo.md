# B4 investigation memo — can a branch activity failure already be captured as a value?

Orchestrator-owned investigation required by `FLOW-VOCAB-BUILD-PLAN.md`
before B4 dispatch. Produced 2026-07-15; appended to the B4 brief at
dispatch time. Fixes B4's scope for `collect ?`.

## 1. Verdict

**Split verdict: mostly pure emitter work, plus one small SDK addition for
exactly one case — the parallel activity fan-out.**

Failure-as-value already exists at every granularity except one:

- **Single activity call**: `workflow.run` returns
  `Result(o, error.ActivityError)` — a retries-exhausted terminal arrives as
  `Error(Terminal(...))`, a plain value
  (`gleam/aion_flow/src/aion/workflow/run.gleam:38-40, 77-81`; the engine
  completion task drives to a *final* outcome incl. retry policy before the
  await delivers — `crates/aion/src/runtime/nif_activity_dispatch.rs:290-309`).
- **Child workflow branch**: `child.await(handle)` returns
  `Result(output, ChildError(e))` per handle
  (`gleam/aion_flow/src/aion/child.gleam:63-65`); the emitter already spawns
  all handles then awaits each in item order
  (`crates/aion-awl/src/emitter/forks.rs:180-204`).
- **Sequential fan-out**: the `try_fold` shape (forks.rs:230-247) trivially
  becomes a per-item `case` into `Option`. Pure emitter.
- **The gap — parallel activity fan-out**: the only parallel-activity
  primitives are `all`/`map`/`race`, and `all` is **engine-enforced fail-fast
  with sibling cancellation**: "fail fast on the lowest-ordinal recorded
  failure … and record `ActivityCancelled` for everything unresolved"
  (`crates/aion/src/runtime/nif_collect.rs:14-18, 485-494`; SDK doc
  `gleam/aion_flow/src/aion/workflow/concurrency.gleam:20-22, 47-50`). Once
  one branch fails, sibling slots are destroyed — `[T?]` is unrecoverable
  from this surface.

**The SDK addition** (aion_flow only; engine untouched): a settle-shaped
combinator in `gleam/aion_flow/src/aion/workflow/concurrency.gleam`,
re-exported from `aion/workflow.gleam`:

```gleam
pub fn map_settled(items: List(a), to_activity: fn(a) -> Activity(i, o))
  -> List(Result(o, error.ActivityError))
// + all_settled, + _with_default twins for the task-queue default seam
```

Semantics: encode + dispatch every member **in item order** (each via the
existing single-dispatch wire, i.e. `run.gleam`'s private `dispatch` +
`ffi.dispatch_activity`), collecting correlation ids; then await each id
**in item order** via
`pump.run(fn() { pump.shield(ffi.await_activity_result(id)) })`; decode
successes with the output codec, parse failures with the existing
`activity_error` parser (run.gleam:249-277). No fail-fast, no sibling
cancellation; each member's retry policy runs to a final outcome
independently. Empty list → `[]` (matches §2 "collect yields `[]`").

This is buildable without engine changes because the dispatch/await split
already exists as separate FFI functions
(`gleam/aion_flow/src/aion/internal/ffi.gleam:17-18, 38-39`) and the engine
keys completions per activity ordinal: awaits resolve per `activity_id` from
keyed runtime maps, wake markers are pure wakes safe to consume for any
await, and replay resolves each ordinal from its recorded terminal
(`nif_activity_dispatch.rs:610-707, 728-757`). It is the exact activity twin
of the child spawn-all-then-await-each pattern the emitter already uses.
(Generated code *could* technically import `aion/internal/ffi` — no
`internal_modules` guard in `gleam/aion_flow/gleam.toml` — but it would have
to reimplement config encoding, error parsing, and the pump/shield suspension
contract, all private to `run.gleam`. Wrong layer; put it in the SDK.)

## 2. Evidence summary

| Fact | Where |
|---|---|
| Spec: `collect ?` = `[T?]`, one slot per item, item order, failure detail in history | `docs/design/aion-authoring/awl/AWL-FLOW-VOCABULARY.md:65-70`; capability claim at `:260-264` |
| `run` returns activity failure as a value | `gleam/aion_flow/src/aion/workflow/run.gleam:38-40, 77-81` |
| `all`/`map` fail-fast + cancel siblings (engine-side) | `crates/aion/src/runtime/nif_collect.rs:14-18, 485-494`; `concurrency.gleam:20-22` |
| dispatch/await are separate FFI calls; awaits keyed per ordinal, out-of-order completion buffered | `internal/ffi.gleam:17-39`; `nif_activity_dispatch.rs:679-707` |
| Child branch failure already a per-handle value; emitter awaits handles in item order | `child.gleam:63-65`; `emitter/forks.rs:180-204` |
| Generated code already *continues after* a captured failure (`on failure` attempt closure + `case`) | `emitter/steps.rs:301-353`; runtime taxonomy `gleam/aion_flow/src/aion/awl/error.gleam:18-28, 124-131` |
| `T?` lowers as `GType::Option` with codecs — `[T?]` is representable today | `emitter/types.rs:35, 119, 146, 237` |
| Strict `collect` ≡ existing `workflow.map` fail-fast (forks.rs:256-261) — no work beyond region plumbing | `emitter/forks.rs:38-119` |

## 3. Sketch — where the absent slot is minted

The `Option` substitution happens in **generated Gleam** (emitter); the SDK
stays `Result`-shaped (keeps error detail for other consumers, incl. future
`on failure` forms); the engine is untouched.

1. **Parallel distribute, single-activity body** (the workhorse, incl.
   `run_agent`): emit
   `let awl_settled = workflow.map_settled(items, fn(item) { <activity value> })`
   then
   `let results = list.map(awl_settled, fn(r) { case r { Ok(v) -> option.Some(v) Error(_) -> option.None } })`.
   Item order: `map_settled` dispatches and awaits in item order, so slot i
   is item i by construction.
2. **`sequence` + `collect ?`**: the existing `try_fold` shape with the
   per-item `result.try(... |> awl_error.map_activity_error)` replaced by a
   `case` into `Option`, accumulate, reverse. Pure emitter, no SDK.
3. **Distribute over child/subflow-shaped per-item bodies**: the existing
   spawn-then-await shape, with each
   `child.await(handle) |> map_child_error` replaced by a `case` into
   `Option` per handle, awaited in handle (= item) order. Pure emitter, no
   SDK.

## 4. The `on failure` tie-in — confirm the capability, refute the dependency

The two refusals are:

- `crates/aion-awl/src/emitter/steps.rs:301-311` — "step `{}` combines
  `on failure` with a body-terminal route — the Gleam stopgap cannot tell a
  routed failure outcome from a step failure there";
- `crates/aion-awl/src/emitter/subs.rs:77-86` — same for substeps.

**Confirmed** that the same capability underlies both:
failure-captured-as-a-value *at the point of the fallible call*. Today's
lowering funnels every body error through `result.try` into one
`Result(_, AwlError)` attempt closure, so a body-terminal route (whose tail
call can itself return `Error(AwlOutcomeFailure(...))` — a *routed failure
outcome*, error.gleam:26) would be indistinguishable from a step failure in
the `Error(_)` arm. Capturing each call's `Result` at the call site (a
`case`, exactly what `collect ?` emission does) discriminates exactly:
compensation triggers only on captured operation failures, and the route
stays a genuine tail expression outside any closure.

**Refuted** that the refusals need the SDK addition: the value already
exists for the single-call shapes these refusals cover (`run`/`child.await`
return `Result` today). Retiring them is pure emitter restructuring;
`map_settled` is needed only for the *parallel fan-out* instance of the
capability. The design doc's "one capability, two constructs" holds at the
capability level, not at the SDK level.

## 5. Risks / unknowns for the implementer to verify live

1. **N outstanding single-dispatch activities awaited individually is an
   unexercised engine path** — `run` couples dispatch+await; `collect_*`
   uses the batch NIF. The plumbing supports it by construction (keyed
   completions, pure wake markers, per-ordinal replay), but nothing runs it
   today. Needs a runtime proof in the `runtime_codecs` harness style
   (`crates/aion-awl/tests/runtime_codecs/fork_generality.rs`) *and* the
   live dev_flow run: dispatch order vs completion order inverted, one
   branch failing terminally, replay after crash mid-settle.
2. **`with_timeout` scope expiry around a settled fan-out**: the batch NIF
   durably cancels all pending members on expiry (`nif_collect.rs:19-20,
   623+`); the per-await path records expiry only for the ordinal *being
   awaited* (`nif_activity_dispatch.rs:695-724`). Members dispatched but not
   yet awaited when a scope expires have no cancellation story — verify, or
   scope timeouts around `collect ?` out explicitly.
3. **In-VM tier members**: `dispatch_activity` refuses `in_vm` on the
   arity-3 wire (`nif_activity_dispatch.rs:38-46`); `map_settled` must route
   in-VM members through the arity-4 wire or refuse them, mirroring
   `ActivitySpec::selects_in_vm` (`nif_collect.rs:50-57`).
4. **History/console story**: spec promises "the run history holds each
   failure's detail" — settled branches record N independent terminals
   rather than the batch's fail-fast + cancellation set; confirm the
   ops-console rendering is sane.
5. **Multi-step parallel region bodies**: the SDK parallelizes activities,
   not statement sequences (recorded stopgap, `steps.rs:174-179`). B4 must
   choose sequential fallback vs child-spawn lowering for those region
   bodies; both have failure-as-value today, but the tolerant-capture code
   shape differs per choice — decide before emitting.
6. **Compensation-vs-outcome discrimination** in the refusal retirement:
   `AwlOutcomeFailure` (a routed `route failure(...)`) must never trigger
   `on failure` compensation — the new per-call-capture shape is the fix;
   add a fixture asserting it.
