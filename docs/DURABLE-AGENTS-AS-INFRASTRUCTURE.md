# Durable Agents as Infrastructure — the Ablative pitch (Tom's vision)

> **Status: north-star pitch, 2026-06-22 (Tom, dictated — he has a fuller written-up version too).**
> This is the framing the whole stack has been building toward. Captured so the work stays pointed at it.

## The line
**AI agents as infrastructure** — not a tool you build *with*, not a useful assistant, but a **load-bearing
primitive you can make guarantees about.** Durable agents you deploy the way you deploy infrastructure.

## Why the Ablative stack can make the guarantee
- **Everything is Rust or Gleam → compile-time safety.** The whole stack is statically checked; correctness
  is provable at build time, not hoped for at runtime.
- **A single binary that never dies.** After the beamr ahead-of-time (AOT) compiler work, you deploy **one
  file** that survives crashes, **shows its work**, and **runs forever** (cf. the "Mercury" shows-its-work-and-
  runs-forever property). Immutable trace logs underneath everything.
- **Durability is structural, not bolted on.** Aion's durable workflows + haematite's immutable/content-
  addressed trace + beamr's OTP supervision mean the agent's progress is persisted and recoverable by
  construction.

## The distributed end-state (deliberately AFTER single-node is "just right")
- Processes die, whole **servers die, you rip the power out of the wall** — and the work is **picked up by a
  sister server in the cluster.** They **auto-deploy and just pick it up.**
- If your **workflow is set up right and your environment is durable** (e.g. you commit + push at regular
  intervals so the working state itself is recoverable), you get a **completely durable agent that never
  stops** — you can deliberately kill/stop it, but it will not *fail* its way to death.
- A stack full of these — durable, supervised, recoverable, trace-logged — is the killer property.

## A concrete shape of it
Deploy something like a **Norn agent that runs in a loop**: pull briefs/steps, **spin off Aion workflows as
needed** to do the work, recover across crashes/redeploys. (This is essentially what the brief-dispatch loop
already does, made into deployable, never-dying infrastructure.) The capacity for that is enormous.

## Sequencing discipline (Tom's explicit constraint)
- **Single-node first, get it genuinely right, THEN distribute.** Do not build the distributed story until
  the single-node primitive is solid.
- **Keep the stack dependencies disentangled — no "inbreeding."** The graph is a **strict linear chain**,
  not a diamond: **beamr → haematite → liminal → aion → norn** (everything depends on beamr; each layer
  depends only on the ones beneath it, in order). Crucially **aion depends on liminal, NOT directly on
  haematite** — liminal is the layer that gives aion its affinity / fan-in / fan-out / messaging patterns,
  so aion's durability flows *through* liminal. No circular or tangled deps; storage never depends on its
  consumers. (Watch item: haematite's `append`/EventStore — keep it consumed by liminal, not by a direct
  aion→haematite edge, or the line becomes a diamond.)
