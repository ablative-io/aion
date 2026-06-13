# aion — Decisions

_Updated: 2026-06-13_

## Decided (9)

### ADR-001 — No arbitrary limits, no assumed defaults

- **Scope:** project · **Date:** 2026-06-13 · **Decided by:** Tom

**Context.** Recorded retroactively at ledger adoption; in force since the project's start (CLAUDE.md coding standards). Engines accumulate hardcoded 'sensible defaults' — caps, rate limits, timeouts, retry policies — that silently bind users who never chose them. The fork: bake in defaults for convenience, or force every configurable value to be chosen.

**Decision.** Configurable values come from the builder/author or are deferred to the layer that owns them (e.g. beamr's own defaults). No caps, rate limits, or hardcoded defaults invented at the aion layer. Values are discussed before implementation. Rejected: convenience defaults — they are decisions made for the user without telling them.

**Consequences:**
- Required arguments where other systems would default (parent-close policy per spawn is required, ADR-004)
- Schemas in the design system carry no default keyword; emptiness is authored explicitly
- Reviews treat a new hardcoded value as a finding regardless of how sensible it looks

### ADR-002 — No backwards compatibility during the build

- **Scope:** project · **Date:** 2026-06-13 · **Decided by:** Tom

**Context.** Recorded retroactively at ledger adoption; in force since the project's start (CLAUDE.md). Pre-1.0, compatibility shims accumulate as zombie code: deprecated markers, parallel code paths, wire-format escape hatches.

**Decision.** Replace, don't add alongside. No compat shims, no zombie code, no #[deprecated] markers. Breaking changes are made cleanly and consumers move forward. Rejected: incremental deprecation cycles — they double the surface under test for an audience of zero.

**Consequences:**
- Wire-format and schema changes break loudly and completely (wire-compat suites assert byte-identical both directions against the CURRENT format only)
- Releases bundle breaking changes into minor version bumps while pre-1.0

### ADR-003 — No default timeouts anywhere in the engine

- **Scope:** project · **Date:** 2026-06-13 · **Decided by:** Tom

**Context.** A hardcoded 30s activity dispatch timeout killed a 58-second norn dev step during the first live dogfood run. The fork: raise the default, make it configurable with a default, or remove engine-imposed bounds entirely.

**Decision.** The engine imposes no activity time bound of its own. Activity waits are unbounded, terminated only by completion, worker loss, server shutdown, or a workflow-level timeout the author explicitly chose. Rejected: a bigger default — agentic activities legitimately run for over an hour, and any number we picked would be ADR-001 violated.

> we shouldn't have a default timeout, the agent steps can take well over an hour
> — Tom, 2026-06-13

**Consequences:**
- Worker-loss detection had to actually deliver (it had never fired in production) — fixed via RAII stream-teardown guard
- Authors who want time bounds set them per workflow; the engine provides the mechanism, never the number
- Shipped in 0.6.0; the fix-loop and gate steps in dev workflows are similarly unbounded (fix-until-clean, not max-N-attempts)

### ADR-004 — Parent-close policy: required per spawn, RequestCancel | Terminate | Abandon

- **Scope:** project · **Date:** 2026-06-13 · **Decided by:** Tom

**Context.** Cancelling a workflow kills only its own process; descendants are left resident — a grandchild parked on a three-month timer outlives its cancelled tree (pinned by nested_workflows_e2e). The fork: pick a system default cascade behaviour, or make the author choose per spawn.

**Decision.** Temporal-style per-spawn parent-close policy — RequestCancel (graceful cascade), Terminate (immediate kill), Abandon (child hands off and keeps running past the parent's terminal; today's behaviour made explicit) — as a REQUIRED argument on child.spawn / spawn_and_wait. Rejected: defaulting to any of the three (ADR-001).

> And I'll accept your recommendation for the parent clothes policy that means that if I'm understanding you correctly that you can decide whether a child workflows stays running if the parent workflow stops running as in like a cannibal almost like it hands off is that right?
> — Tom, 2026-06-13

**Consequences:**
- Engine: propagate on ALL parent terminals (not just cancel), recursively; recovery must re-arm pending propagations
- SDK signature change for child spawning (breaking, per ADR-002)
- workflows.md child section and templates update when it lands

### ADR-005 — Failed runs are terminal; recovery resumes Running runs only

- **Scope:** project · **Date:** 2026-06-13 · **Decided by:** Tom

**Context.** Recorded retroactively at ledger adoption (decided during the first dogfood wave). When a run fails, should the engine allow resuming/retrying it in place, or is failure final? Hit live when a failed dogfood run could not be recovered.

**Decision.** A failed run is a terminal, immutable record. Retry means a fresh `aion start` with a new run identity; recovery after restart resumes Running runs only. Rejected: in-place retry of failed runs — it would rewrite history that event-sourcing exists to preserve, and 'which attempt was this event from' becomes unanswerable.

**Consequences:**
- Dispatch tooling mints fresh runs per attempt (test-bed practice: bump brief_id between runs where inputs derive identities)
- Post-mortems read terminal runs via describe; querying terminal runs errors by design

### ADR-006 — Multi-reviewer verdicts: votes aggregate in Meridian, one signal decides

- **Scope:** project · **Date:** 2026-06-13 · **Decided by:** Tom, with Waffles

**Context.** stacked_dev supports multiple reviewers (all DM'd), but the workflow has a single review_verdict signal and meridian review complete accepts a single vote. The fork: N verdict signals with quorum logic inside every workflow, or aggregation outside with one decision signal.

**Decision.** The workflow keeps a single review_verdict signal as THE decision. Reviewers vote via `meridian review complete --verdict`; the Meridian coordinator applies the quorum policy and fires the one aion signal. Rejected: per-reviewer signals into the workflow — quorum policy is a Meridian concern, and every workflow re-implementing vote-counting is the wrong layer. Integration seam: Meridian needs the branch→workflow-id mapping at review-request time.

> keeping the same signal thing but allowing... casting the votes... to Meridian and then Meridian provides the signal
> — Tom, with Waffles, 2026-06-13

**Consequences:**
- Meridian-side: coordinator + branch→workflow-id registry (rides the re-pin wave)
- aion-side: nothing — the single-signal contract is already live-proven

### ADR-007 — Design system v2: JSON ledgers above clusters, enrichment in place

- **Scope:** project · **Date:** 2026-06-13 · **Decided by:** Tom

**Context.** v1 standardised cluster documents but had nothing above the cluster (where work comes from, what was decided) and execution records lived in workflow outputs, separate from the briefs they executed. The forks: ledgers vs. ad-hoc roadmap prose; execution records appended into the brief vs. a sibling runs/ ledger.

**Decision.** Two project ledgers (roadmap.json, decisions.json) above the clusters; stage contracts as first-class schemas inside the aion codegen subset; the brief is one living document — the pipeline appends scout/dev/review per requirement and an execution block per brief, in place, never touching authored fields. Rejected: a sibling runs/ ledger — the brief as a single spec-plus-record document was the original intent, and aion's event history already provides the append-only audit trail.

> basically stuff is just depended to it so I would actually be happy to have it to have it saved back in place from where it came from
> — Tom, 2026-06-13

**Consequences:**
- docs/design-system/ holds schemas, guides, scripts; extracted to its own repo when the next project (messaging bus) starts
- Workflow codecs for stage payloads are generated from the same schemas authors validate against
- check-roadmap.py enforces that ledger status claims carry their artifacts

### ADR-008 — brief_dev replaces onatopp_dev inside the stacked-dev family

- **Scope:** brief-dev · **Date:** 2026-06-13 · **Decided by:** Tom

**Context.** The v2 pipeline (scout → dev → verify → adversarial review → harden) needs a home. The stacked-dev family's inner child onatopp_dev is a scout-less, review-less dev loop — exactly the thing the pipeline supersedes. The fork: evolve the family in place, or build a sibling family alongside it. Tom accepted the replacement while noting he'd also have been comfortable keeping both temporarily; ADR-002 tips the balance to replace.

**Decision.** Evolve in place: onatopp_dev.gleam is deleted and brief_dev.gleam takes its slot as stacked_dev's inner child; the outer arc keeps its live-proven contracts. Rejected: a parallel brief-dev family — two families serving one purpose is the zombie-code pattern ADR-002 prohibits, and the outer arc's provision/gate/review/land contracts took a full dogfood night to prove against real CLIs; duplicating them duplicates that risk.

> so brief Dev replacing on a top Dev in the stacked dev family. I guess that's fine but like I also don't mind sort of holding onto both for the time being
> — Tom, 2026-06-13

**Consequences:**
- StackedDevInput reshapes (v2 brief document + resolved context replace the four document strings) — breaking, family redeploys as a unit
- Meridian's rhai onatopp-dev-norn is unaffected until their own migration (RM-015 re-pin first)
- The dev-pipeline template mirrors the replacement in the same wave

### ADR-009 — Enrichment rides the worktree branch and lands with the merge

- **Scope:** brief-dev · **Date:** 2026-06-13 · **Decided by:** Tom

**Context.** Stage reports must be appended into the brief document in place (ADR-007). But WHERE does the write happen while a run is in flight? The brief lives in the repo; the run works in a provisioned worktree on a stacked branch. The fork: enrich the main-tree brief from outside the run, enrich a separate store and merge later, or enrich the worktree copy so the record travels with the code.

**Decision.** The enrich_brief activity writes the brief file inside the run's worktree; the enriched brief is committed by land alongside the implementation and arrives on main in the same merge. Rejected: main-tree writes from a running workflow (races concurrent runs and pollutes main with in-flight state) and a separate execution store (re-creates the spec/record split ADR-007 closed; aion's event history already serves as the append-only store).

> then the second one I agree with
> — Tom, 2026-06-13

**Consequences:**
- A failed/rejected run leaves NO enrichment on main — its record lives only in the workflow's durable event history (describe), which is the correct asymmetry: main carries the record of what landed
- Re-runs after rejection start from the authored brief again (failed runs are terminal, ADR-005)
- The execution block is written before land so the landed commit contains its own provenance
- The execution block cannot contain its own landing commit hash (a commit cannot name itself): landed_commit stays empty in the riding record; the workflow's event history and the merge itself carry the hash
