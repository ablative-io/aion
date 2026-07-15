# B3 — projection + authoring canvas for the flow shape

Contract: `AWL-FLOW-VOCABULARY.md` rev 3 §6 (the diagrams are the
acceptance picture). Areas: `crates/aion-server/src/awl/projection.rs`
(+ wire types via ts-rs regen) and
`apps/aion-ops-console/src/features/authoring/` (projection types,
canvas components, layout). Depends on B2 (consumes its semantic-index
step kinds).

## Objective

The authoring canvas draws what the document says, 1:1 — every step a
node, every node a step.

1. **Projection**: replace the flattened `markers: {looped, forked}`
   model. `ProjectionStep` gains `kind` (plain / distribute / sequence
   / collect / subflow_call / decision), region membership (which
   distribute opened the region this step is in, if any), the
   distributed binding and collection text for labels, the subflow name
   + a nested `GraphProjection` for subflow declarations, and visits
   bounds for cycle labels. Backward routes already project as edges —
   keep them, label them with the bound (`×N`).
2. **Canvas**:
   - distribute/sequence nodes with the split glyph and the
     `item in collection` label; sequence visually distinct from
     distribute;
   - steps inside a region marked ×N (banding or badge — operator
     reviews);
   - collect nodes with the merge glyph and the `binding → name`
     label (`?` shown);
   - subflow-call nodes collapsed by default, expandable in place to
     the subflow's own graph (reuse the canvas recursively);
   - pure-decision steps drawn as diamonds; mixed steps keep the node
     with a trailing diamond for their outcome arms;
   - cycle back-edges drawn with their `×N visits` label.
3. **Editor seam**: node selection still maps to source spans
   (stepAtPosition) for the new kinds; diagnostics tones unchanged.

## Scope out

Run-view (workflow-detail) rendering. Emitter. Any server behavior
beyond the projection shape.

## The bar

- Unit tests on the projection builder: the rev-3 dev_flow fixture
  projects to exactly five parent nodes with the right kinds, one
  nested subflow graph, one region, one back edge with `×3`.
- Component tests (existing SSR string-render convention) for each new
  node kind and the expansion toggle.
- Typecheck, biome, bun test green; ts-rs wire types regenerated and
  committed together with the console changes (embed regen stays with
  the orchestrator at fold time).
- **Operator checkpoint**: after merge, the operator reviews the canvas
  against rev 3 §6 before B4 is folded. Layout judgment calls go to the
  operator, not into code review threads.
