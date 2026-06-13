# Workflow-Toolkit — User Stories

## Workflow Author — Standing up a new agentic family

**S1.** As a workflow author, I want to define a new agentic family by writing a PipelineConfig — my prompts, my stage schemas, my gate commands — rather than a from-scratch workflow module, so that a new family costs a day of configuration instead of a brief-set of bespoke plumbing.

**S2.** As a workflow author, I want to import the provision/build/check/land harness instead of re-deriving the yg/cargo/git invocations, so that I inherit the live-proven worktree-and-land behaviour without re-discovering its hazards.

**S3.** As a workflow author, I want my workflow code to stay thin by default — threading sealed payloads and reading only facts — so that I don't reintroduce the per-replay heavy-decode regression every new family would otherwise copy.

**S4.** As a workflow author, I want aion new to scaffold the thin harness-and-pipeline shape, so that the cheap pattern is the starting point rather than something I have to refactor toward.

## AI Team Member — Dispatching and re-dogfooding the pipeline

**S5.** As an AI team member, I want the reshaped pipeline to land a real brief end to end through norn after the change, so that I can dispatch work knowing the lighter pipeline still completes the whole arc, not just type-checks.

**S6.** As an AI team member, I want a multi-brief dogfood run to survive a kill -9 of the server and resume without re-executing completed activities, so that a long run is never lost and the thin-workflow reshape is proven durable under crash.

## AI Lead — Reviewing the reshape at the code gate

**S7.** As an AI lead reviewing the reshape, I want to confirm by grep and by type signatures that the workflow decodes no full report and renders no prompt, so that I can verify the determinism boundary holds without re-reading every stage by hand.

**S8.** As an AI lead, I want to confirm the outer stacked-dev contracts are byte-unchanged, so that I can let the inner reshape through without re-proving the provision/gate/review/land arc that took a full dogfood night to land.

**S9.** As an AI lead, I want the lifted harness invocations pinned byte-identical to the originals, so that I can trust the extraction is a move and not a silent behaviour change.

## Norn Agent — Receiving a stage prompt

**S10.** As a norn agent, I want my prompt rendered on the worker that dispatches me — from the brief, the resolved context, and the prior stages — so that I receive the same resolved, budgeted prompt as before even though the workflow no longer builds it.

## Future Maintainer — Extending the toolkit later

**S11.** As a future maintainer, I want the dev-pipeline skeleton and the worker-harness to live in named reusable modules rather than inlined in one example, so that I can add the next family or fix the harness in one place instead of N copies.

**S12.** As a future maintainer, I want the facts projection documented as the negative space of the old in-workflow decode, so that when I add a new routing signal I know it must come from a fact the workflow already needed, not a freshly decoded report field.

## Tom — Approving the design-level calls

**S13.** As Tom, I want the agents' output kept exactly as it is — sealed and passed to the worker which unpacks it and builds the prompt — so that the reshape moves where decoding happens without changing what the agents produce or how the briefs are enriched.

**S14.** As Tom, I want the standard library kept to genuinely cross-cutting primitives — the payload, templating, data wrangling — with the norn agent harness staying worker-side, so that the toolkit doesn't ship a Meridian-specific runtime to every consumer.
