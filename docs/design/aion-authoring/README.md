# Aion authoring (doc index)

The authoring experience: the AWL language, the studio, and the decision history
that led here. If you're new: **awl/AWL-BIG-PICTURE.md** for where this is going,
**VISUAL-AUTHORING-SURFACE.md** for the studio, then the decision history below
for why.

## Current (load-bearing)

| Doc | What it is |
|---|---|
| [awl/](awl/README.md) | **The language** — spec, vocabulary, IR, tooling, big picture. Has its own index. |
| [VISUAL-AUTHORING-SURFACE.md](VISUAL-AUTHORING-SURFACE.md) | The authoring studio surface of record (canvas, deploy, run, observe). |
| [AWL-FACADE-CONTRACT.md](AWL-FACADE-CONTRACT.md) | The facade contract between the language core and the studio. |
| [DISPATCH-RUNBOOK.md](DISPATCH-RUNBOOK.md) | How authoring-arc build work is dispatched and folded. |

## Decision history (June–July 2026, kept for the record)

Chronological; each fed the next. These explain *why* AWL exists and looks the
way it does — the format-must-smell-like-the-product ruling, the death of the
YAML-likes, the surface rethink.

| Doc | What it decided |
|---|---|
| [DESIGN.md](DESIGN.md) + [design.json](design.json) | The original authoring-experience design pass. |
| [USER-STORIES.md](USER-STORIES.md) + [stories.json](stories.json) / [CHECKLIST.md](CHECKLIST.md) + [checklist.json](checklist.json) | Requirements baseline (June). |
| [COMPETITIVE-RESEARCH-2026-07-02.md](COMPETITIVE-RESEARCH-2026-07-02.md) (+ raw json) | The field survey (Temporal/Restate/Inngest/etc. authoring). |
| [AUTHORING-MODEL-DISCUSSION-2026-07-02.md](AUTHORING-MODEL-DISCUSSION-2026-07-02.md) | The authoring-model debate. |
| [REVIEW-AND-RECOMMENDATIONS-2026-07-03.md](REVIEW-AND-RECOMMENDATIONS-2026-07-03.md) / [USABILITY-FINDINGS-2026-07-03.md](USABILITY-FINDINGS-2026-07-03.md) | Review + usability findings on the early direction. |
| [SURFACE-RETHINK-2026-07-03.md](SURFACE-RETHINK-2026-07-03.md) | The surface rethink that preceded the own-language decision. |
| [syntax-sketches/](syntax-sketches/README.md) | The candidate syntaxes (A–H). **H won** and became AWL; the others are the record of what was rejected and why. |

## briefs/

Dispatched build briefs for the studio arc (WA-001..007 and successors), with
inputs. Historical dispatch records.
