# docs/archive

Historical design docs for **completed or superseded** work, moved here (2026-07-02)
to declutter the `docs/` root. Nothing here is deleted — it's the build record for
work that has shipped. Each doc self-labels its status (most say `✅ LANDED /
IMPLEMENTED / SUPERSEDED / ARCHIVED`). The live design surface is the
`docs/design/<cluster>/` folders + `docs/design/roadmap.json`; current forward designs
stay at the `docs/` and `docs/design/` roots.

Archived because the work is done or the framing was overtaken:

| Doc | Shipped as |
|---|---|
| AION-DISTRIBUTION-REVIEW.md | design-input for the distribution wave (caveats built, #157) |
| AION-OUTBOX-BUILD-PLAN.md / AION-OUTBOX-CUTOVER-DECISION.md | outbox (#9–13) + cutover |
| DISTRIBUTED-ROUTING-DESIGN.md / ROUTING-MODEL.md | superseded by NSTQ + node-affinity + control-plane |
| LIMINAL-SWAP-DESIGN.md / LIMINAL-WORKER-SUBSCRIPTION-DESIGN.md / XNODE-DELIVERY-DESIGN.md | the LSUB cross-node dispatch track (#101–112) |
| MULTI-SHARD-ACTIVE-ACTIVE-DESIGN.md | AA-4-x multi-shard (#29–35) |
| NAMESPACE-TASKQUEUE-SPLIT-DESIGN.md | NSTQ (#84–89) |
| NODE-AFFINITY-DESIGN.md | NODE-1..5 (#91–95) |
| STORAGE-SWAP-MATURITY-DESIGN.md | substrate landed; framing overtaken by control-plane / haematite-SoT |
| TIMER-CANCELLATION-FIX-PLAN.md | timer fixes (761be22e) |
| WORKFLOW-REOPEN-DESIGN.md | reopen / `aion reopen` (#174) |
| IMPLEMENTATION-TRACKER.md | the original brief-wave tracker (superseded by roadmap.json) |
| CLI-UX-AND-DISTRIBUTED-DIRECTION.md | dated session friction-log (actioned → control-plane) |
| SESSION-2026-06-15-FOLLOWUPS.md | dated session log |
