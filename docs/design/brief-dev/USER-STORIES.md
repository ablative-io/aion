# Brief-Dev — User Stories

## Tom — Approving and steering work others dispatch

**S1.** As Tom, I want a wave dispatched by anyone — usually an AI team member — to reach me only at its review gates so that getting work done means approving documents, not babysitting terminals.

**S2.** As Tom, I want each brief's review gate to DM its assigned reviewers — usually AI leads, me for design-level calls — with the workflow parked until the verdict signal so that the decision point is explicit without me being the bottleneck.

**S3.** As Tom, I want a dispatch run to survive kill -9 of the server at any point so that a long multi-brief wave never restarts from scratch.

**S4.** As Tom, I want a wave to refuse briefs whose dependencies haven't landed so that ordering mistakes are caught at dispatch, not discovered as broken builds.

## Claude — Authoring briefs and operating the pipeline

**S5.** As Claude, I want stage payloads validated against the same schemas I author briefs against so that a malformed document breaks at dispatch with a pointer, not mid-run in an agent prompt.

**S6.** As Claude, I want the live status query to name the current stage and round so that I can watch a run without parsing event histories.

**S7.** As Claude, I want a failed run's full enrichment preserved in its event history so that I can post-mortem exactly what the scout found and the dev claimed before the failure.

## Norn Agent — Executing a pipeline stage

**S8.** As a norn agent, I want my prompt to carry the resolved requirement texts, anchored decisions, and the humans' verbatim words so that I act on what was actually asked without re-deriving context.

**S9.** As a norn agent resuming after a crash or a fix round, I want my same session resumed by deterministic id so that I keep my context instead of rediscovering the codebase.

## Reviewer Agent — Adversarial verification of a dev round

**S10.** As a reviewer agent, I want the dev's attestation alongside the measured check results so that I can treat their divergence as a signal about where to dig.

**S11.** As a reviewer agent, I want a fresh session separate from the dev's so that I verify the diff with my own eyes instead of inheriting the dev's framing.

## AI Team Member — Initiating, monitoring, and chaining workflows

**S14.** As an AI team member, I want to assemble and dispatch a wave end to end — ledger read, reference resolution, aion start — without a human in the loop so that briefed work starts the moment its dependencies land.

**S15.** As an AI team member, I want dispatch to return machine-readable per-brief outcomes so that I can file follow-up roadmap items for failures and chain the next wave without human triage.

**S16.** As an AI orchestrator, I want to watch a wave through status queries by workflow id so that I can start dependent work the moment a prerequisite brief lands rather than polling git.

**S17.** As an AI team member, I want the dispatcher to refuse a stale or coverage-broken brief at assembly time so that I cannot accidentally start a run against documents that drifted since authoring.

## Future Maintainer — Reading landed work months later

**S12.** As a future maintainer, I want the landed brief to carry what was asked, what the scout found, what the dev did and why, and what review proved so that provenance is one file away from the code.

**S13.** As a future maintainer, I want gate results and agent attestations recorded separately in the execution block so that I can tell measured truth from believed claims.

## AI Lead — Reviewing code at the review gate

**S18.** As an AI lead reviewing code at the review gate, I want the review request and the enriched brief on the branch to carry everything a rigorous review needs — per-criterion acceptance evidence, measured gate results, attestation divergence — so that I can let nothing through without re-deriving context.

**S19.** As an AI lead, I want the review_verdict signal to be sender-agnostic so that my verdict, cast through the Meridian coordinator, decides the run exactly as a human's would.
