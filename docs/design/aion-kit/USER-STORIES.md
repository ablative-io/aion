# Aion-Kit — User Stories

## Activity Author — Writing worker-side activity bodies that build prompts and wrangle reports

**S1.** As an activity author, I want composition primitives that join sections, lines, bullets, and key-value pairs and drop the empty ones so that I can build a prompt from parts without re-deriving the joiners or hand-handling blank gaps every family.

**S2.** As an activity author, I want a named-template render that fills '{name}' placeholders from explicit bindings so that a static prompt skeleton can carry its holes visibly instead of being assembled by string concatenation.

**S3.** As an activity author, I want to seal a large typed result into an opaque payload and to unseal one I receive with my own decoder so that I do the heavy decode once, on the worker, rather than forcing the workflow to open it.

**S4.** As an activity author, I want to deep-merge a stage report into a document append-only and order-stable so that enrichment is a library call with right-wins leaf semantics rather than a bespoke per-family merge.

## Workflow Author — Writing deterministic workflow code that threads activity results

**S5.** As a workflow author, I want to hold an activity result as an opaque payload and pass it straight to the next activity without decoding it so that my deterministic workflow heap stays small and replay never re-decodes a large report.

**S6.** As a workflow author, I want to peek only the few facts I need to route control flow (pass/fail, blocked, changed files, drift) out of an opaque payload so that I can branch on a result without materialising its whole structure.

**S7.** As a workflow author, I want every toolkit primitive to be pure and deterministic so that I can call the same projection and JSON helpers from workflow code or activity code without risking a replay divergence.

## Future Maintainer — Reading and extending the toolkit months later

**S8.** As a future maintainer, I want the toolkit scoped to cross-cutting primitives with the runtime harnesses kept out so that I can tell at a glance what belongs in the standard library and what is a consumer's own concern.

**S9.** As a future maintainer, I want every primitive's negative cases (an absent projection field, a merge leaf conflict, a missing path, an unmatched placeholder) covered by tests with typed results so that I can change an implementation and trust the suite to catch a regression.

## Meridian Consumer — Integrating the kit into their own workers

**S10.** As a Meridian engineer building a worker, I want to depend on aion_kit and use its templating, JSON wrangling, and payload helpers without it dragging in the norn agent driver so that the toolkit couples to no agent runtime and I integrate my own harness on top.

**S11.** As a Meridian engineer, I want the opaque payload type to live in the authoring SDK my workflows already import so that threading a sealed result between activities needs no extra dependency in my deterministic workflow code.
