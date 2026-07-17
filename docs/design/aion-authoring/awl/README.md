# AWL — the Aion Work Language (doc index)

Read in this order if you're new: **AWL-BIG-PICTURE.md → AWL-2-SPEC.md →
AWL-FLOW-VOCABULARY.md**. Everything else is build history or specialist depth.

## Current (load-bearing)

| Doc | What it is |
|---|---|
| [AWL-BIG-PICTURE.md](AWL-BIG-PICTURE.md) | Direction map (2026-07-17): identity ("Aion **Work** Language"), thesis, cross-boundary type checking, supervised work, whole-stack opportunities. Deliberately not a spec. |
| [AWL-2-SPEC.md](AWL-2-SPEC.md) | **The language spec of record** — grammar, types, semantics as built. |
| [AWL-FLOW-VOCABULARY.md](AWL-FLOW-VOCABULARY.md) | The flow vocabulary (distribute/collect, visits, child, on failure, substeps) as designed and shipped (landings 1–6, all live). |
| [AWL-BC-IR.md](AWL-BC-IR.md) | The MIR / bytecode-path intermediate representation — the direct MIR→BEAM emitter's ground truth. |
| [AWL-EDITOR-TOOLING-SPEC.md](AWL-EDITOR-TOOLING-SPEC.md) | Editor tooling surface (LSP, grammar). Built: hover, goto-def, formatting, tree-sitter grammar. Not built: autocomplete. |
| [BRIEF-CRAFT.md](BRIEF-CRAFT.md) | How build briefs for AWL work are written (the dispatch discipline). |

## Build history (kept for the record; superseded or completed)

| Doc | Status |
|---|---|
| [AWL-0-SPEC-DRAFT.md](AWL-0-SPEC-DRAFT.md) | First spec draft (2026-07-09). Superseded by AWL-2-SPEC.md. |
| [AWL-EXECUTION-PLAN.md](AWL-EXECUTION-PLAN.md) | The original AWL build sequencing. Executed. |
| [AWL-2-BUILD-PLAN.md](AWL-2-BUILD-PLAN.md) | Lexer/parser/printer build plan. Executed (landed on main). |
| [AWL-BC-DESIGN-DRAFT.md](AWL-BC-DESIGN-DRAFT.md) | Early bytecode-path design draft. Superseded by AWL-BC-CODEC-DESIGN.md + AWL-BC-IR.md. |
| [AWL-BC-CODEC-DESIGN.md](AWL-BC-CODEC-DESIGN.md) | Bytecode codec design (BC arc). |
| [AWL-BC-BUILD-PLAN.md](AWL-BC-BUILD-PLAN.md) | BC arc build plan. Executed (one-motion deploy/run landed). |
| [AWL-UX.md](AWL-UX.md) | Authoring UX exploration feeding the studio build. Largely realised; see VISUAL-AUTHORING-SURFACE.md (parent dir) for the surface of record. |
| [FLOW-VOCAB-BUILD-PLAN.md](FLOW-VOCAB-BUILD-PLAN.md) | Flow-vocab landing plan (B1–B5). Executed — all landings merged and live. |

## Subdirectories

- **briefs/** — dispatched build briefs + inputs for the AWL arcs (WA-*, AWL-*,
  BC-*, flow-vocab-B*), including the B4 investigation memo and salvage records.
- **exam/** — the authoring exam/playtest protocol and ledger (candidate pack,
  workbench, feedback schema).
- **examples/** — example .awl documents (rev2 = current revision).

## Related (outside this directory)

- [../../WORKER-AUTHORING-STORY.md](../../WORKER-AUTHORING-STORY.md) — worker
  file kind, namespaced vocabularies, batteries-included workers (2026-07-17).
- [../../WORKER-DEPLOYMENT.md](../../WORKER-DEPLOYMENT.md) — deployment/
  placement/supervision machinery design.
- [../VISUAL-AUTHORING-SURFACE.md](../VISUAL-AUTHORING-SURFACE.md) — the studio.
- beamr/docs/AOT-NORTH-STAR.md — the tree-shaking AOT long arc.
