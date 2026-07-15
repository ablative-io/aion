# B2 — flow shape: subflow, distribute/sequence, collect, visits

Contract: `AWL-FLOW-VOCABULARY.md` rev 3 §1–§3, §5. Crate:
`crates/aion-awl` (parser, ast, checker, semantic index, printer).
Depends on B1 (same files; branch from its merge).

## Objective

Grammar + checker + semantic index for the ratified flow shape. After
this brief, `aion awl check` accepts and enforces the rev-3 surface;
nothing lowers yet (emitter refuses these constructs with a clear
"not yet lowered" diagnostic, NOT a parse error).

1. **`subflow` declarations** — anatomy identical to `workflow` (typed
   inputs, exactly ONE success outcome, own steps); invocation is a
   step statement binding the outcome type. Subflows nest. No capture
   of enclosing bindings — parameters only.
2. **`distribute <var> in <collection>`** and
   **`sequence <var> in <collection>`** — each the ONLY line of its
   step. Opens a per-item region; `<var>: T` in scope downstream for
   `[T]` collections.
3. **`collect <binding> -> <name>`** and **`collect <binding>? -> <name>`**
   — opens its step; strict form types `[T]`, tolerant form `[T?]`
   (slot-per-item, item order). Further statements/outcomes after it
   are legal.
4. **Region rules (the meat — get these right):**
   - every distribute/sequence reaches exactly one collect; every
     collect closes the nearest open region (bracket nesting; a region
     opened inside a region must close inside it);
   - the collected binding is definitely assigned on every success
     path through the region;
   - routes inside a region may not target steps outside it; the only
     exit is the collect;
   - loop-backs (backward routes) inside the region are legal and stay
     inside;
   - empty collection is legal (collect yields `[]`).
5. **`max N visits`** — step attribute; `visits` builtin `Int` in that
   step's outcome guards. The route-cycle rule becomes: a cycle is
   legal iff a member step carries a visits bound or an input-derived
   bound — and a bounded `loop` inside a member NO LONGER satisfies it
   (closes the decoy-loop soundness gap; adjust the two existing
   diagnostics' wording).
6. **Decision tagging** — a step whose body is empty with only
   outcome lines is classified `decision` in the semantic index; steps
   get a `kind` (plain / distribute / sequence / collect / subflow_call
   / decision) so B3 can project without re-deriving.

## Scope out

Emitter/MIR lowering (B4). Projection/canvas (B3). Intra-step
`fork`/`join` keep parsing unchanged (deprecation diagnostics are B4's
migration tail).

## The bar

- Checker tests per region rule, positive and negative, asserted by
  diagnostic line:column — including nested regions, region loop-backs,
  escape attempts, unassigned collected bindings, decoy-loop cycles now
  rejected.
- Printer round-trip property tests over all new syntax.
- The rev-3 §6 dev_flow document (full new surface) is a fixture that
  PASSES check; at least six mutation fixtures (one per region rule)
  FAIL with the right diagnostic.
- Gates: cargo fmt / build / clippy -D warnings / test, exit codes
  recorded.
