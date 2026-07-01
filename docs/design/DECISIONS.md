# aion — Decisions

_Updated: 2026-06-29_

## Decided (31)

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

### ADR-010 — The reviewer prompt excludes the scout

- **Scope:** brief-dev · **Date:** 2026-06-13 · **Decided by:** Tom

**Context.** The review stage projects a prompt for the adversarial reviewer. As first built it rendered the scout findings alongside the dev record per requirement (the original C9). The question Tom raised reviewing the projections: does the reviewer actually need the scout?

**Decision.** The reviewer prompt renders the brief, the dev record per requirement, the dev attestation, and the measured check results — never the scout. The scout is the devs orientation, not the reviewers input; including it spends prompt budget and risks biasing the verification with the devs framing. The reviewer verifies the devs actual diff against the brief with fresh eyes (its own session, CN4).

> the review it doesnt really need anything from the scout the reviewer needs the output of the dev step and the initial brief and that kind of stuff and it needs to be also formatted and laid out in a sensible way
> — Tom, 2026-06-13

**Consequences:**
- review_prompt drops its scout parameter; brief_dev no longer threads the scout report into the review activity.
- Checklist C9 amended: the review projection renders only the dev blocks, the attestation, and the measured checks.
- A test asserts the scout approach text is absent from the review prompt.

### ADR-011 — The standard library carries cross-cutting primitives only; runtime harnesses stay out

- **Scope:** aion-kit · **Date:** 2026-06-13 · **Decided by:** Tom

**Context.** Reshaping brief_dev surfaced a pile of plumbing — templating, data transformation, opaque pass-through, and the norn agent driver — that could be lifted into a worker standard library so future families cost a day, not a week. The fork: a batteries-included library that bundles the agent driver too, or a lean library of only what generalises. Tom drew the line at the agent driver: it is the norn runtime harness he wrote weeks ago, specific to one agent runtime, and a thing Meridian consumes rather than a universal primitive.

**Decision.** aion_kit (the worker standard library) carries only primitives that apply across the board — data transformation/wrangling, rendering/templating, and the opaque payload — plus further cross-cutting primitives as they prove general. Runtime harnesses, the norn agent driver chief among them, stay as worker-side code that consumers (Meridian) own and integrate; they may live in a worker for convenience but never ship as standard-library surface. Rejected: bundling the norn/agent harness into the standard library — it couples a general toolkit to one runtime and ships a Meridian concern to every consumer.

> Norn's too specific. Like it's Norn at the runtime, the agent harness, like I wrote that a couple of weeks ago. So like it's not even like I don't think we should ship that as part of the standard library. That would be more something that Meridian would consume.
> — Tom, 2026-06-13

**Consequences:**
- aion_kit scope is template + json/data-wrangling + payload; new additions are judged by the across-the-board test, not convenience
- The norn agent driver stays in the worker layer; Meridian consumes and integrates it, and is free to keep it somewhere nice and easy
- The dev-pipeline template (RM-023) parameterises the agent step rather than baking a runtime into the toolkit

### ADR-012 — Workflow code stays thin: large activity results ride as opaque payloads

- **Scope:** project · **Date:** 2026-06-13 · **Decided by:** Tom

**Context.** The brief_dev pipeline decoded full structured stage reports (a 12KB scout report) and rendered prompts inside the deterministic workflow process. That re-decodes on every replay, bloats the workflow heap, and — via a latent beamr put_list/put_tuple2 heap-reservation bug, fixed in beamr 0.6.1 — crashed a real dogfood run with a silent heap-full right after scout. The fork: keep the heavy decode/render in workflow code where the data already sits (convenient, single place), or push it out to the worker.

**Decision.** Workflow code stays thin. Large activity results ride between stages as opaque sealed payloads the workflow never opens; the workflow decodes only a small facts projection — the few fields it needs to route control flow (pass/fail, changed files, blocked, drift). Decoding and prompt rendering happen in the consuming activity on the worker, which has a full heap and runs the work once rather than on every replay. Rejected: decoding and rendering full reports in workflow code — it puts the determinism boundary's heavy lifting in the wrong place and was the regression that exposed the beamr crash.

> we're not changing the output of the agents right we're still keeping all of that which is not decoding it all within the workflow so it also stays packaged up and then it passes out to the worker the worker unpacks it and provides the prompt to the agent
> — Tom, 2026-06-13

**Consequences:**
- aion_flow gains an opaque pass-through payload type a workflow can hold without decoding (RM-022); aion_kit gains payload helpers (seal / raw / peek)
- brief_dev is reshaped to thread sealed payloads plus thin facts, and prompt rendering moves into the activity bodies (RM-023)
- Replay cost drops — no per-replay re-decode or re-render — and the workflow heap stays small regardless of report size
- New agentic families inherit the thin pattern by default through the dev-pipeline template

### ADR-013 — Reopen reuses the existing single-writer Recorder; no new lock primitive

- **Scope:** aion-durability · **Date:** 2026-06-16 · **Decided by:** Tom

**Context.** The reopen operation (AD-012) must append WorkflowReopened to a terminal-Failed workflow and respawn it, and invariant #3 requires exactly one Recorder per workflow across that whole sequence. Two situations exist: the workflow failed in this server's lifetime (its handle is still registered, residency Suspended) or it failed before a restart (no handle exists at all). The fork: reuse the existing per-workflow write lock, or introduce a new global per-workflow reopen-lock primitive to serialise reopen attempts. Raised because Tom initially read 'single writer' as conflicting with the engine's massive-concurrency goal; clarified that invariant #3 is per-workflow (one writer for one workflow's event log, a requirement of deterministic replay), while concurrency is between workflows (millions of independent processes) and even within a workflow the activity work still fans out — only the per-workflow log append is serialised.

**Decision.** The reopen operation reuses the existing per-workflow write lock — the handle's recorder Arc<Mutex<Recorder>> — serialised at creation by the registry mutex via an atomic insert-if-absent (a Suspended handle carrying a Recorder::resume_at at the history head is created when none exists, else the existing one is reused). One continuous Recorder is held from the WorkflowReopened append through the respawn; register_recovered_resident is parameterised to accept that injected recorder so reopen and startup recovery share one respawn-and-register path. Rejected: a dedicated global reopen-lock map — it adds a second concurrency mechanism when the recorder mutex already IS the per-workflow write lock, and the recorder's expected-sequence discipline already turns a racing double-reopen into a hard SequenceConflict (safety), so a lock would only add graceful serialisation, not correctness.

> I think I agree with your recommended approach
> — Tom, 2026-06-16

**Consequences:**
- register_recovered_resident is refactored to accept an externally-held Recorder; the refactor must be behaviour-preserving for startup recovery
- Invariant #3 is reaffirmed for reopen and is explicitly per-workflow, not global — cross-workflow concurrency is unaffected
- REVISIT (deferred, not decided): the single-writer-per-workflow model may be re-examined once the liminal messaging bus (improved-NATS, fan-in/fan-out) and the hematite storage engine (multi-reader/multi-writer, embedded), both built on beamr, mature — they could change the per-workflow concurrency and append story. Flagged here as a deliberate come-back-to; no change is made now.

### ADR-014 — Authoring: the typed Gleam module is the single source of truth; no separate DSL

- **Scope:** aion-authoring · **Date:** 2026-06-21 · **Decided by:** Tom

**Context.** Shipping one activity today means writing it in five to seven places that must agree byte-for-byte (the activity.new wrapper and its name, the local body, the call site, the codec pair, the worker handler, the worker registration, the workflow.toml entry, and for typed workers a hand-derived wire-compat golden). The recurring instinct — recorded as RM-009 (declarative DSL + visual builder) and gestured at whenever authoring friction bites — is to answer this with a separate declarative DSL or a visual builder as the authoring surface. The industry is split: Restate, Inngest, and DBOS stay code-first with explicit named side-effect blocks, while a class of tools offer visual builders as the source. The fork: introduce a separate DSL or manifest as the source of truth, or keep the typed Gleam module as the single source and generate, observe, and project everything else from it.

**Decision.** The typed Gleam workflow-plus-activities module is the single source of truth. The activity signature already carries the contract (its input and output types, codecs, and tier); the engine generates the rest — worker handler stubs, registration, the manifest activities list, codecs, schemas, wire-compat goldens, and test skeletons — and projects the rest — the diagram and the time-travel state. No standalone textual DSL becomes a parallel authoring surface, because it would forfeit the compile-time type safety that is Aion's core differentiator (the aion-flow guarantee that an invalid composition fails gleam build). A visual canvas is a generated projection of the typed source with a live execution overlay and bounded structural round-trip, not a second source. Rejected: a separate DSL or manifest as the source of truth — it re-creates the spec-vs-implementation drift the typed module already eliminates and trades a strong static guarantee for a weaker one.

> ADR-014 is ratified
> — Tom, 2026-06-21

**Consequences:**
- RM-021 (declare-once codegen), RM-009 (reframed as a visual projection), and the new aion-authoring roadmap items (RM-025..RM-029) are bound by this principle.
- The activity declaration form drives worker, manifest, codec, schema, and test codegen the way I/O schemas already drive codec codegen (ADR-007 precedent); aion gains an `aion generate` surface extending today's I/O-only `aion codegen`.
- No standalone DSL runtime is built; the visual canvas is a projection of the typed source, never the authoritative artifact.
- Ratified by Tom on 2026-06-21; the aion-authoring cluster's designed roadmap items (RM-021, RM-009, RM-025..RM-029) are cleared to be briefed and dispatched.

### ADR-015 — Dashboard: URL is state — every meaningful view state is deep-linkable

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** Operators hand off incidents mid-triage. A console whose view state lives in ephemeral component memory cannot be shared, bookmarked, or restored — the receiver re-navigates from scratch, and back/forward break. Temporal's console is the negative example: little of its view state is URL-addressable. The fork: keep view state in component memory (simpler to build) or make every meaningful state URL-encoded and router-driven (more constraint, but shareable).

**Decision.** Every meaningful view state — namespace, selected workflow, selected event/bar, scrub seq, active search query and filters, active view — is URL-encoded and router-driven; component-local state holds only ephemeral UI (hover, transient focus). Rejected: ephemeral in-memory view state — it forfeits shareable/bookmarkable/back-forward-correct navigation, which is the 3 a.m. handoff primitive (paste a link in Slack, land on the exact bar at the exact scrub point).

> Yes absolutely... it's meant to out class temporal so the UI needs to outlast anything in that area. Add that stuff to the vision document and anywhere else it's needed.
> — Tom, 2026-06-29

**Consequences:**
- The router is the single source of view state; every new view must define its URL schema as part of its design.
- Operator-action URLs (reopen/cancel) may address a target but never auto-execute — the action still requires an explicit confirm (ADR-013 / VISION §7).
- VISION §1 (professional-console bar) and §6.5 (URL as state) are bound by this; reviewers check that no shareable state is trapped in component memory.

### ADR-016 — Dashboard: the console never lies about its own provenance/freshness

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** After a read fails over to a survivor, or when a socket drops, a console that silently shows stale or wrong-node data misleads an operator under pressure — the most dangerous moment to be misled. The fork: assume-fresh (simpler UI, no provenance plumbing) vs always-surface provenance + freshness.

**Decision.** The dashboard always surfaces its own data provenance and freshness: which node it is reading from, the last seq it has applied, and an explicit 'viewing a survivor' signal after a read fails over. A silently stale or post-failover view is a defect, not a graceful degradation. Rejected: assume-fresh / render data without provenance — under failover that turns the console into a confident liar exactly when truth matters most.

> Yes absolutely... it's meant to out class temporal so the UI needs to outlast anything in that area. Add that stuff to the vision document and anywhere else it's needed.
> — Tom, 2026-06-29

**Consequences:**
- Every read path carries source-node + last-applied-seq provenance through to the UI; the connection/staleness indicator is non-negotiable (VISION §6.3).
- Depends on the server exposing source-node identity and a seq cursor on reads/streams (coordinate with the AW contract and the §8 failover-event promotion).
- Reviewers check that no view can present stale/old-node data without an honest indicator.

### ADR-017 — Dashboard: keyboard-first with a command palette as the spine

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** The consoles operators reach for under pressure — k9s, Linear, Superhuman — are dense and keyboard-driven; Temporal is sparse and mouse-bound. The fork: mouse-primary with keyboard as an accessibility afterthought, or keyboard-first with a command palette as the universal entry point.

**Decision.** Keyboard-first: a command palette (universal Cmd-K / '/' entry) is the fastest path to everything — jump to a workflow, run a search, switch namespace, issue an action — and every primary surface is fully keyboard-navigable, with context-sensitive palette actions. Mouse stays fully supported, but keyboard is never a second-class path. Rejected: mouse-primary / keyboard-as-afterthought — it cedes speed-of-triage, a direct outclass vector over Temporal.

> Yes absolutely... it's meant to out class temporal so the UI needs to outlast anything in that area. Add that stuff to the vision document and anywhere else it's needed.
> — Tom, 2026-06-29

**Consequences:**
- A global command-palette + keybinding system is core architecture from the start, not bolted on later (VISION §4.6).
- Every view defines its keyboard navigation model and its palette action contributions as part of its design.
- Palette actions that mutate state route through the command API and the confirm boundary (VISION §7), never direct writes.

### ADR-018 — Dashboard: performance is a budgeted, measured feature

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** 'Faster than logs' is meaningless without numbers. A console that janks at 10k events, or blocks on first paint, loses operator trust precisely when speed matters. The fork: best-effort performance vs committed, measured budgets treated as correctness.

**Decision.** Performance is a committed contract: the swimlane stays smooth at 10k+ events via virtualized lanes while live events append; the triage surface paints the top incident within one screen on first load; list and search results stream/virtualize rather than block. Budgets are measured, not merely asserted, and a regression past them is treated as a defect. Rejected: best-effort performance — it lets the console degrade into a slower-than-logs tool under real load, defeating its entire premise.

> Yes absolutely... it's meant to out class temporal so the UI needs to outlast anything in that area. Add that stuff to the vision document and anywhere else it's needed.
> — Tom, 2026-06-29

**Consequences:**
- Virtualized rendering is required for the swimlane, list, and search from the start (not retrofitted); large-history handling is designed in.
- Performance budgets become explicit review/CI criteria (VISION §6.6).
- The swimlane/scrubber bespoke rendering layer must be built against these budgets.

### ADR-019 — Dashboard: the healthy/calm state is designed, not an empty page

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** A triage-first console risks shipping only the broken-state design and leaving the healthy 99% case as an empty or spinning page; the tool then only feels finished when something is on fire. The fork: triage-only, or an equally-designed ambient healthy state.

**Decision.** The healthy/ambient cluster-heartbeat state — nodes live, shards owned, workers connected, throughput nominal — gets the same design care as the incident cards; the operator who glances at a calm cluster sees a legible 'all clear', not emptiness. Rejected: triage-only / undesigned healthy state — it leaves the console feeling unfinished in its most common condition and erodes trust in the quiet.

> Yes absolutely... it's meant to out class temporal so the UI needs to outlast anything in that area. Add that stuff to the vision document and anywhere else it's needed.
> — Tom, 2026-06-29

**Consequences:**
- The three-AM view (VISION §4.4) must render a designed calm state, reading the same topology/throughput signals positively.
- Empty/healthy is a first-class design state alongside loading/error/incident; reviewers check it exists and is legible.

### ADR-020 — Dashboard: a live cluster map is a first-class view; design its command seam alongside the event seam

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** Operators need a whole-system mental model of the distributed cluster, and the failover beat today is a counter, not motion. The push-channel work (#128/WS3) is being designed right now, so the question is whether to (a) build only an observe map fed by those events, or (b) also design a command seam now so the map can DRIVE operator actions later. A force-directed graph is the tempting-but-wrong default. Temporal-class consoles are observe-mostly; a map that previews the consequence of an action before it is taken is a different class of tool.

**Decision.** Build a deliberate-layout (NOT force-directed) live cluster map as VISION concept 4.7 — nodes/shards/workers/in-flight workflows, with work as tokens flowing along the real Liminal dispatch edges and failover rendered as motion. It elevates the single-purpose /failover view into the general view. Tier 0 (navigate/drill, deep-linked) and Tier 1 (cluster time-scrub) ride on the cluster-event push channel and are Phase 1. Tier 2 (operator actions: cancel/reopen, dead-letter redrive, DRAIN a node, PLANNED shard handoff, chaos kill-node) is the control-plane north-star but is ASPIRATIONAL: each action mutating cluster state requires a NEW named server command that does not exist today, runs strictly as a command the engine's single writer enacts (invariant 3), and is gated behind explicit confirm + a blast-radius preview. Decision: design the event seam (animates the map) AND the command seam (acts from it) TOGETHER in WS3 so the map is fed and operable from one coherent contract, even though Tier-2 commands ship incrementally as each lands. Rejected: a force-directed graph as the primary layout (decorative, illegible under pressure, violates the hand-plane principle); and an observe-only map (forgoes the control-plane leap that is the actual durable-agents-as-infrastructure vision).

> I think I agree with you for your sort of assessment. I just would like it all captured and you make sure that we don't confuse it with the final state.
> — Tom, 2026-06-29

**Consequences:**
- WS3 (#128) must carry BOTH a cluster-event push channel and a command seam; the push-channel design is no longer event-only.
- Tier-2 actions are spec'd-but-disabled until their named server commands (drain, planned handoff, redrive) exist; they are explicitly NOT part of the Phase-1 baseline (VISION s9 'Phase 1.5 — Control plane').
- Blast-radius preview before any state-mutating action is a requirement, not polish (the reopen-diff idea applied to cluster ops); reuses the s7 command boundary and s6.3 provenance.
- VISION s4.7 carries an explicit status marker separating already-true direction (Tier 0/1) from aspirational (Tier 2) so the spec is never mistaken for the shipped state.

### ADR-021 — Adoption: clean-partial multi-shard adoption (commit what you won)

- **Scope:** aion · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** Fixing the double-adoption race (#122/WS2), a survivor adopting multiple shards after an owner dies can win the fenced publish for some shards and be deposed (NotOwner) on others within the same adoption. The behavior for that mixed outcome must be defined; it shapes the failover semantics demonstrated to others.

**Decision.** Clean-partial: recover and serve the shards whose fenced publish succeeded, and cleanly drop (do NOT recover/serve, leave owned_shards scope unwidened for) the shards that were fenced, retrying those on a later supervisor tick. A typed deposition (NotOwner) is a clean drop, never a hard error; only quorum-unavailable (ElectionTimeout/QuorumTimeout) stays a retryable error. Rejected: all-or-nothing fail-closed (any single fenced shard aborts the entire adoption with no recovery) — it trades availability for atomic simplicity, and the whole point of failover is that work keeps flowing.

> I think I agree with you for your sort of assessment.
> — Tom, 2026-06-29

**Consequences:**
- adopt_shards_inner becomes a per-shard acquire+publish pipeline with a committed accumulator; extend_owned_shards + recover run only over committed shards, after a re-assert of is_current_owner.
- The WS2 test matrix must prove win-A/fenced-B leaves B absent from owned_shards, unrecovered, and not re-spawned, while A is recovered.
- Requires the haematite typed-fence split (Fenced vs CasConflict) + is_current_owner from haematite 0.3.0 so 'deposed' is distinguishable from a benign value-CAS race.

### ADR-022 — Dashboard: three-tier RBAC capability model (read / per-workflow-command / cluster-control)

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** Today authorization is `deploy: bool` + namespace grants. ADR-020's control-plane spec would conflate 'can deploy a package' with 'can drain/kill a node' — a deploy token would imply cluster-kill authority, which is wrong. The fork: keep the flat deploy-bool model and overload it, or split authorization into capability tiers before any M2 command can land. This shapes claim fields and the command-seam contract, so it must be settled first.

**Decision.** Adopt a three-tier capability split: READ (observe everything), PER-WORKFLOW-COMMAND (cancel/reopen — reuses the existing namespace-ownership guard), and CLUSTER-CONTROL (drain/handoff/kill — a separate, high grant). A deploy token must NOT imply cluster-kill authority; this replaces the flat `deploy: bool` + namespace-grants model. The split shapes claim fields and the command-seam contract and must land before any M2 command. Rejected: overloading the existing deploy boolean — it conflates package deployment with node-destroying cluster control, the exact authority leak this model exists to prevent.

**Consequences:**
- Claim fields are reshaped to carry the three capability tiers; the flat deploy-bool is removed (ADR-002 clean replace).
- Per-workflow commands reuse the namespace-ownership guard already in the engine; cluster-control is a distinct high grant.
- Must land before any M2 command lands, because it defines the command-seam contract (couples to ADR-020's command seam and S4).

### ADR-023 — Dashboard: control-action safety via idempotency-key + wait-for-effect-event + pinned blast-radius

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** A command client that optimistically writes history or executes against a stale view is dangerous under failover and concurrency. The fork: optimistic UI (write the effect locally, reconcile later) vs a strict round-trip discipline that mirrors the engine's fencing/CAS model. This is hard to retrofit once the command client is built.

**Decision.** Control actions use an idempotency-key plus WAIT-FOR-EFFECT-EVENT reconciliation: the UI NEVER optimistically writes history — it waits for the server's event confirming the effect. Additionally, a server-computed BLAST-RADIUS preview is pinned to `cluster_seq` with optimistic-concurrency so a stale preview cannot execute. This mirrors the engine's fencing/CAS model. Rejected: optimistic history writes in the UI — under failover and concurrency they make the console assert effects that may not have happened, the opposite of ADR-016's provenance honesty.

**Consequences:**
- The command client is built around idempotency-keys and effect-event reconciliation from the start (no optimistic local history).
- Blast-radius previews are pinned to `cluster_seq`; a stale preview is rejected by optimistic-concurrency at execute time.
- Couples to ADR-020 (blast-radius before any state-mutating action) and ADR-016 (provenance/freshness).

### ADR-024 — Dashboard: dual-mode swimlane axis (logical seq-rank default + temporal-width toggle)

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** The swimlane is the centerpiece view. Seq-rank ordering preserves the partial-order / ShiViz correctness argument but makes bar width meaningless; operators instinctively expect width to mean elapsed time. The fork: pick one axis (correctness vs operator expectation) or support both. This is L effort but it is the centerpiece, so the choice matters.

**Decision.** DUAL-MODE: logical seq-rank is the default (preserves the partial-order / ShiViz correctness argument), with a temporal-width toggle that maps width to operator-expected duration. Both modes are first-class. Rejected: a single fixed axis — seq-rank-only forfeits the duration intuition operators reach for; temporal-only forfeits the partial-order correctness guarantee that makes the swimlane trustworthy.

**Consequences:**
- The swimlane rendering layer supports two axis mappings and a toggle; URL-encodes the active mode (ADR-015).
- L effort accepted on the centerpiece view; both modes must hold the ADR-018 performance budget at 10k+ events.
- Relates to U6 in the scope doc.

### ADR-025 — Dashboard: light theme delivery deferred to Phase 1.5; token architecture built to best practice now

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** Temporal ships both themes; light theme is real work (toggle + a light-theme audit of status colors under the hand-plane constraint). The fork: ship light theme in Phase 1, or defer delivery while still ensuring the token architecture can absorb it later without a component refactor. Tom's explicit, strong requirement: adding light mode later must be a token-map addition, never a component rewrite.

**Decision.** Light-theme DELIVERY is deferred to Phase 1.5 (ship dark-first, polished). BUT the design-TOKEN ARCHITECTURE is built to best practice NOW: multi-tier semantic tokens, fully theme-swappable, so adding light mode later is a TOKEN-MAP ADDITION, never a component refactor. Both theme maps live at the token layer — dark = shipped, light = defined-now / delivered-1.5. Rejected: a quick dark-only token setup that hardcodes theme decisions into components — it would turn light mode into a painful refactor later, which Tom explicitly forbade ('I really don't want it to become a nightmare later').

> I really don't want it to become a nightmare later.
> — Tom, 2026-06-29

**Consequences:**
- Token architecture is three-tier (primitives → semantic → optional component), single-source-of-truth, theme-swappable, authored now and documented in DESIGN-TOKENS.md.
- The light theme map is DEFINED now (at the token layer) but DELIVERED in Phase 1.5; dark ships first.
- Components reference semantic tokens only (no raw hex, no opacity modifiers); a CI guard enforces it. Relates to U4.

### ADR-026 — Dashboard: TLS via required terminating proxy for M1; refuse non-loopback plaintext in prod

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** The console serves operator-facing control surfaces; plaintext over a network is unacceptable in production. The fork: implement in-process rustls termination now, or require a TLS-terminating proxy in front and refuse to serve plaintext on non-loopback addresses in prod. In-process rustls is more self-contained but more surface to own at M1.

**Decision.** For M1: TLS is proxy-required (a TLS-terminating proxy in front), and the server REFUSES non-loopback plaintext in production. In-process rustls is kept as a later option, not an M1 obligation. Rejected: building in-process rustls termination for M1 — it adds TLS-management surface to own at exactly the moment the priority is shipping the console safely; a required proxy + plaintext refusal achieves the safety guarantee now.

**Consequences:**
- Production refuses to bind plaintext on non-loopback addresses; a TLS-terminating proxy is a documented deployment requirement.
- In-process rustls remains a deferred option for a later phase.
- Relates to S12 in the scope doc.

### ADR-027 — Dashboard: server keepalive frames drive freshness (connected-but-silent downgrades freshness)

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** After a socket goes quiet, a client cannot distinguish 'healthy and idle' from 'silently stale' without a signal. The fork: a client-side last-frame-time heuristic (no server work, but guesses) vs server KEEPALIVE FRAMES plus the server stamping node + last-applied seq on responses (needs server work, but authoritative). This feeds ADR-016's provenance promise.

**Decision.** Use server KEEPALIVE FRAMES (plus the server stamping node + last-applied seq on responses) rather than a client heuristic. This feeds ADR-016 provenance: a connected-but-silent socket DOWNGRADES freshness, so the console is honest about going stale even when the connection is technically alive. Rejected: a client last-frame-time heuristic — it guesses at staleness instead of being told, and cannot reliably distinguish idle-healthy from silently-stale, undermining the ADR-016 honesty guarantee.

**Consequences:**
- AW contract must add server keepalive frames + node/last-applied-seq response stamping (couples to D3/A6 resync-floor work).
- A connected-but-silent socket is rendered as degraded freshness, not 'fresh' (ADR-016).
- Relates to D4 in the scope doc; decide before S9.

### ADR-028 — Dashboard: virtualization via @tanstack/react-virtual

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** The 10k-event swimlane and large lists/search results need real windowing to hold the ADR-018 performance budget; server-paging alone does not solve the swimlane. The fork: adopt a virtualization library, or rely on strict server-paging as the list 'virtualization story'. A library aligned with the existing TanStack stack reduces integration risk.

**Decision.** Use `@tanstack/react-virtual` (house-stack aligned with the existing TanStack Query usage). Rejected: strict server-paging as the sole virtualization story — it does not window the 10k-event swimlane, which needs real client-side virtualization regardless to meet the ADR-018 budget.

**Consequences:**
- `@tanstack/react-virtual` is added as a dependency; the swimlane, list, and search use it for windowing.
- Aligns with ADR-018 (virtualized rendering required from the start) and D7.
- Confirmed dependency add.

### ADR-029 — Dashboard: cluster-map fallback ships last-known state with honest provenance if WS3 slips

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** The live cluster map (ADR-020 Tier 0/1) rides the WS3 cluster-event push channel, which is in flight. If WS3 slips, the console must not block on it. The fork: block the console until WS3 lands, ship the single-purpose failover view as the cluster surface, or ship coarse last-known cluster state derived from existing query data with honest provenance and upgrade later.

**Decision.** If WS3 slips, ship COARSE LAST-KNOWN cluster state derived from existing query data with honest 'last-known' provenance, and UPGRADE to the live map when WS3 lands. Never block the console on WS3. Rejected: blocking the console on WS3 (forfeits a usable cluster surface for an external dependency) and shipping the bare failover view alone (less general, and still leaves the healthy cluster view undesigned).

**Consequences:**
- A last-known cluster surface is built from existing query data with explicit provenance (ADR-016) and upgrades to the live map (ADR-020) when WS3 lands.
- The console's Phase-1 boundary does not depend on WS3 landing on time.
- Relates to C7 and the Phase-1 cluster-map boundary in the scope doc.

### ADR-030 — Dashboard: audit goes to a real durable sink before any M2 command carries authority

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** `tracing::info!` is not an audit store — it is lossy, unstructured, and not queryable as a record of who did what. Once M2 commands carry real authority (cancel/reopen/drain/kill), there must be a durable, tamper-evident record, including denials. The fork: keep logging-as-audit, or stand up a real durable sink before commands carry authority.

**Decision.** Audit goes to a real DURABLE sink — a dedicated `audit` namespace or a hash-chained log — before any M2 command carries authority; DENIALS are recorded too, not just successes. `tracing::info!` is explicitly not an audit store. Rejected: logging as the audit store — it is lossy and not a trustworthy record, unacceptable once the console can mutate cluster state.

**Consequences:**
- A durable audit sink (dedicated `audit` namespace or hash-chained log) is built before M2 commands carry authority.
- Denied actions are audited alongside successful ones (RBAC denials per ADR-022 are recorded).
- Relates to S6 in the scope doc.

### ADR-031 — Dashboard: event-reference docs auto-generated from ts-rs doc-comments, CI-guarded for zero drift

- **Scope:** aion-dashboard · **Date:** 2026-06-29 · **Decided by:** Tom

**Context.** The event reference can be hand-curated (richer prose) or auto-generated from the ts-rs doc-comments that already define the wire types. Hand-curated docs drift from the actual types; generated docs stay in sync but need wiring. The wire-types CI guard already exists to prevent type drift.

**Decision.** AUTO-GENERATE the event reference from ts-rs doc-comments, wired into the existing wire-types CI guard so there is ZERO drift between the documented events and the actual wire types. Rejected: hand-curated event docs — richer prose is not worth the inevitable drift from the real types, especially when the wire-types guard already gives us a zero-drift mechanism to hook into.

**Consequences:**
- Event-reference generation reads ts-rs doc-comments and is wired into the existing `wire-types-no-diff` CI guard.
- Documented events cannot drift from the wire types; a drift fails CI.
- Relates to G3 in the scope doc.
