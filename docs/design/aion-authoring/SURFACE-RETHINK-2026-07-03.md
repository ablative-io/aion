# Authoring Surface Rethink — 2026-07-03

Capture of Tom's late-session thinking on the workflow authoring surface,
recorded before the Meridian handoff. This REOPENS the surface question:
**sketch G (YAML-carrier step document) is NOT ratified.** The canonical-model
architecture underneath is unchanged and unchallenged — model is the source of
truth, surfaces are lossless views, types live on the actions. What's back on
the table is what the human/AI-facing surface looks like.

Read alongside `syntax-sketches/` (A–G) and
`AUTHORING-MODEL-DISCUSSION-2026-07-02.md`.

## The YAML unease (why G isn't landing)

Tom's first workflow-engine implementation used actual YAML as the authoring
format and it was a genuinely bad experience ("couldn't be less appropriate in
its native form"). Sketch G only *borrows* YAML's syntax — real-YAML carrier,
our semantics, schema-validated — and in theory that kills the risk. Tom
acknowledges the theory and still doesn't feel it: "I worry I'm retreading
exactly the same mistakes." A surface the owner is uneasy about is the wrong
surface, whatever the theory says. So: broader conversation required before
anything is built.

## The candidates as Tom currently feels them

### Nix-shaped (sketch D, resurrected)

Keeps coming back to him unprompted: "not quite JSON, not quite JavaScript —
but also actually a language that is understood." Points in favour he named:
real language with real tooling/highlighting/priors, expression-native so the
`when:`/`do:` boundary problem dissolves. (Counterweights from the original
sketch verdict: unfamiliar to most humans, laziness/fixpoint semantics we'd
have to explicitly NOT implement, and AI priors for nix are thinner than for
JS/YAML/markdown.)

### Multi-language front-ends over the canonical model

If the model is the compile target and humans never hand-edit it, the model
format "can be however we want" — and the *surface* can be plural: write your
workflow in TypeScript, or Starlark, or a step document, and it converts to
the model. This is already our architecture (surfaces are views), but the new
emphasis is: maybe there is no single blessed surface at all — "bring your
syntax," with the model + `aion check` as the invariant layer. Cost: every
front-end is a real parser/extractor with real edge cases; each one multiplies
maintenance. Realistic shape: ONE blessed authoring surface + the model
documented well enough that third parties can target it.

### TOML, reconsidered

The rejection was mechanical: TOML 1.0 inline tables can't span lines, so any
nested JSON-ish value goes on one line — unusable for schemas/step bodies. Tom's
instinct: "TOML syntax but easier on the JSON" — i.e. a TOML-flavoured carrier
that relaxes the multiline-value restriction — would *feel* better to him than
YAML even though he can't fully justify why. (Prospekt's PD-001 already chose
TOML for MODEL.toml with full-`[section]` house style; there's family
consistency available here.)

### JSON5/JSONC + multi-file

JSON was rejected as an authoring surface, but JSON5 (comments, trailing
commas) plus a **multi-file architecture** revives it: schemas authored as
sibling JSON Schema files (the worker↔workflow contract, one file per shape),
the workflow document referencing them. Tom's observation about JSON's grain:
it's *certain* — an array has exactly five elements. Workflows increasingly
aren't: his document-processing example — hand in a 5-page doc, one agent is
enough; hand in a 100-page doc with seven chapters, fan out seven agents. The
surface must express "fan out over a runtime-computed list", which literal
JSON arrays fight against. (Note: this cuts against every static surface
equally — G handles it as fan-out-over-expression; any chosen surface needs
the same primitive.)

### Markdown (the "really fucking stupid fucking crazy idea" — his words)

The one he circled longest. Precedent: his pre-aion **"Shapes"** system — a
single markdown document defining an entire system via primitives (roles,
documents, controls, plans, tasks…): effectively prospekt + aion rolled into
one file. Two load-bearing lessons from it:

1. **Original interpretation model**: no deterministic parser — the document
   carries its own interpretation instructions, and a small ephemeral LLM
   (haiku-class) reads "this section just changed" and files the change into
   the database. Semantic, not syntactic, parsing. (In practice a
   deterministic parser got built anyway and "didn't work too bad.")
2. **Why it failed**: everything folded into one 2000-line document / one
   system, no room to breathe. The current stack fixes that by separation —
   aion, prospekt, chiron each their own system — so the *authoring format*
   isn't automatically damned by Shapes' failure.

The new form of the idea: a workflow IS a markdown document. Headings are
steps; bullets are behaviour; the `about:` prose that G bolts on is *native*
here — the document is primarily prose that carries structure, rather than
structure that carries prose. Schemas live as sibling JSON Schema files,
referenced by ordinary markdown links (the SKILL.md pattern: single
self-contained file OR broken out with links — author's choice). Gleam code
blocks embed in-VM logic directly (Tom did the Gleam language tour yesterday;
Gleam-in-markdown for pure steps appeals to him); a Rust worker declares its
actions the same way. Everything is commentable, diffable, readable by
construction, and it is the most AI-native authoring surface that exists —
agents read and write markdown better than anything else.

Risks he named himself: "too loosey-goosey" — the freedom-of-everything
problem; parsing discipline (though far simpler than Shapes: one workflow per
document, schemas externalized); and the "syntax not language" principle
inverts — markdown has no expression syntax at all, so expressions must live
in designated inline code/fields anyway (same expression-grammar question as
G, relocated).

## Constants that survive regardless of surface choice

- Canonical model, lossless parse↔print, canvas = another editor.
- Types on the actions; schemas generated or authored as JSON Schema — never
  duplicated.
- Determinism by construction: the only world-touching verb is calling an
  action.
- A small typed expression grammar exists SOMEWHERE (conditions, bindings,
  fan-out sources) — every candidate needs it; the choice is where it lives.
- Dynamic fan-out (compute the list, fan out over it) is a first-class
  primitive.
- `about:`/prose per step is load-bearing (canvas labels, docs render, live
  ops-console narration).
- AI authors are primary; surface must be schema-checkable with sharp,
  file/line-quality errors (`aion check`).

## Status

- Decision OPEN. Tom wants a broader conversation (to happen via Meridian).
- Nothing gets built against G in the meantime.
- The cheapest next pressure test is unchanged: write agent-dev's real
  workflow in the top 2–3 candidate surfaces side by side (G, markdown,
  nix-shaped) and read them cold.
