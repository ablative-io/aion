# B4 — lowering: MIR + emitter for the flow shape, dev_flow end-to-end

Contract: `AWL-FLOW-VOCABULARY.md` rev 3 §7. Crate: `crates/aion-awl`
(mir, emitter, compile). Depends on B2. Gated on the orchestrator's
failure-as-value investigation memo, which fixes this brief's scope for
`collect ?` before dispatch (pure emitter vs small SDK addition — the
memo's conclusion is appended to this brief at dispatch time).

## Objective

Every rev-3 construct direct-compiles; the dev_flow rewrite runs live.

1. **Regions** — distribute/sequence + collect lower onto the existing
   fan-out machinery (proven since fork-generality); `sequence` uses
   the existing sequential branch delivery. Multi-step regions with
   internal loop-backs lower as per-instance bounded recursion over the
   region's step graph.
2. **Subflows** — inline functions (continuation nesting, the proven
   technique); invocation binds the outcome; nesting works.
3. **Step cycles with `max N visits`** — thread the visit counter;
   exceeding the bound is the same runtime step-failure taxonomy the
   loop `max` uses today.
4. **`collect ?`** — per the investigation memo: branch failure arrives
   as an absent slot (`[T?]`, item order). If the memo concluded an SDK
   addition is needed, that addition is IN this brief's scope, and the
   two `on failure` direct-compile refusals must fall with it (same
   capability; retire the refusal fixtures by making them compile and
   behave).
5. **Const/raw-string/json/schema-of** expressions (B1) reach the
   emitter as folded strings — confirm with a compile proof, no new
   lowering.

## Scope out

Corpus migration (operator ruling: none). Canvas. `fork` retirement
happens here only as: intra-step `fork`/`join` gain a deprecation
diagnostic; removal is a later, separate act.

## The bar

- Real compile proofs (the AWL method): each construct in a minimal
  document → check → emit → `gleam build` exit 0, kept as fixtures.
- The rev-3 §6 dev_flow document compiles on the direct path and is
  committed with its generated module.
- **The live proof**: dev_flow (new surface) deployed and RUN against
  the tharsis scratch repo through the general worker on queue
  `general` — plan → distribute → build (subflow ×N) → collect →
  fold → route back → done, completing with the coordinator's summary.
  Unrun is unproven; this brief is not done until the run is.
- Engine/server untouched unless the memo says SDK addition — and then
  only that addition, test-covered.
- Gates: cargo fmt / build / clippy -D warnings / test (workspace) +
  the compile-proof fixtures, exit codes recorded.
