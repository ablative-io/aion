# aion — Roadmap

_Updated: 2026-06-13_

## Briefed (4)

### RM-017 — brief_dev + dispatch workflow family

- **Kind:** feature

The all-norn inner dev pipeline as an aion workflow (scout → dev → firsthand gate with fix-until-clean resume → adversarial review → harden → re-gate, progressive in-place enrichment, deterministic sessions), composed under stacked_dev, plus a dispatcher workflow that selects briefed work from roadmap.json, orders by dependencies, fans out children with explicit parent-close (ADR-004), serializes lands.

> I would love it if you could write a workflow that that were not just right I mean write a workflow that sort of handles the pattern so goes through like and we try to do it all as much as we could with norn via the workflow.
> — Tom, 2026-06-13

- **Links:** cluster `brief-dev`; decisions ADR-008, ADR-009; briefs BD-001, BD-002, BD-003, BD-004, BD-005, BD-006, BD-007
- **Depends on:** RM-016
- **Notes:** Briefed 2026-06-13, coverage clean. ADR-008/ADR-009 DECIDED by Tom same day — blocked_by on BD-003/004/005 cleared. BD-003..005 must land as ONE wave (the family doesn't compile between BD-003's module deletion and BD-005's rewire). Implementation in flight.

### RM-022 — aion_kit: the worker standard library

- **Kind:** feature

A lean, runtime-agnostic toolkit that ships with the worker so authoring a new family is prompts + schemas + gate, not re-derived plumbing. Three cross-cutting primitives: rendering/templating (text composition + named templates, generalised from prompts.gleam), data transformation/wrangling (project, merge, pluck over JSON), and the opaque payload (seal / raw / peek) plus the aion_flow pass-through type a workflow can hold without decoding. Excludes runtime harnesses (ADR-011); the payload is the keystone of thin-workflow (ADR-012).

> what I think we should keep the standard library to are things that are genuinely to genuinely apply like across the board kind of thing. So, you know, yeah, data transformations, wrangling. Rendering, templating, that kind of stuff.
> — Tom, 2026-06-13

- **Links:** cluster `aion-kit`; decisions ADR-011, ADR-012; briefs KIT-001, KIT-002, KIT-003
- **Notes:** Foundation for RM-023's thin reshape. KIT-001 (opaque payload + package skeleton + the aion_flow pass-through type) is the only intra-cluster dependency; KIT-002 (template) and KIT-003 (json/transform) are disjoint files, parallel-runnable after it. Advances RM-021.

### RM-023 — Thin-workflow reshape + dev-pipeline toolkit

- **Kind:** feature

Reshape brief_dev to ADR-012 — thread sealed payloads plus thin facts, move prompt rendering and report decoding out of the deterministic workflow process into the activity bodies (via aion_kit) — then extract the reusable worker-harness (provision / warm-build / scoped+full checks / land via yg) and the dev-pipeline template (scout → dev → verify → review → harden, parameterised by prompts + schemas + gate) so a new agentic family is configuration, not bespoke code. Re-dogfood after the reshape to prove the lighter pipeline still runs.

> I don't want every workload to take that long. Like do we need more sort of like out of box packages of workers or something like that to run with it to take the pain off
> — Tom, 2026-06-13

- **Links:** cluster `workflow-toolkit`; decisions ADR-012, ADR-008; briefs WT-001, WT-002, WT-003
- **Depends on:** RM-022, RM-017
- **Notes:** Depends on aion_kit (RM-022). The WT briefs all touch examples/stacked-dev, so they run SERIALLY within the cluster, after aion_kit lands. The outer stacked-dev contracts stay immutable (brief-dev CN8). Advances RM-021.

### RM-024 — No silent workflow-process failures: surface crashes loudly

- **Kind:** fix

A workflow-process crash (e.g. the brief_dev heap-full before beamr 0.6.1) recorded a terminal WorkflowFailed event but logged nothing in the server or the worker — to an operator it looked like a hang, especially with no default timeouts (ADR-003). Surface workflow-process exits and crashes at error level in the server log (workflow id, type, the VM message), and propagate a child's terminal failure visibly to the parent run and to watchers (describe / dashboard), so a crash never again masquerades as a hang.

> there's no indication in the server logs that it failed. There was no indication on the worker that it failed... it would look like it was hanging if I didn't go in and actually investigate. We need to do something about that.
> — Tom, 2026-06-13

- **Links:** cluster `aion-observability`; briefs OBS-001, OBS-002
- **Notes:** Disjoint from RM-022/023 (Rust: aion-server + engine + worker), so a clean concurrent run alongside them — part of the parallel stress batch.

## Designed (2)

### RM-001 — Implement parent-close policy

- **Kind:** feature

Required per-spawn RequestCancel | Terminate | Abandon on child.spawn / spawn_and_wait; propagation on all parent terminals, recursive, with recovery re-arming pending propagations. SDK + docs + template updates ride along.

- **Links:** decisions ADR-004
- **Notes:** Engine-wide semantics — wants a cluster design before briefing, not a direct brief.

### RM-015 — Multi-reviewer verdict coordinator

- **Kind:** feature

Reviewers vote via meridian review complete; the Meridian coordinator applies quorum and fires the single review_verdict signal. aion-side contract already live-proven; the work is Meridian-side plus the branch→workflow-id mapping seam at review-request time.

- **Links:** decisions ADR-006
- **Notes:** Implementation lives in the yggdrasil/Meridian repo and rides their re-pin to published aion 0.6.0 + hex aion_flow 0.4.0 (pins currently 88 commits behind at rev 489be454).

## Idea (17)

### RM-002 — Proof portfolio: every public claim has an executable receipt

- **Kind:** process

Claims ledger (docs/CLAIMS.md), public CI on fresh clones, chaos gate (random-kill harness asserting byte-identical history), recorded demos, published benchmark numbers, honest Temporal side-by-side. Credibility-per-effort ordered; CI is the keystone.

- **Links:** (none)
- **Notes:** Do not claim multi-node scale-out — we cannot demonstrate it.

### RM-003 — CLI JSON ergonomics: polymorphic --input, client-side validation, skeletons

- **Kind:** feature

--input/--payload accept inline JSON or @file interchangeably; aion start validates input against the deployed package's schema client-side with RFC 6901 pointers before dispatch; aion input <workflow_type> emits a valid skeleton. A directory form (@dir/ assembling input by mapping schema fields to files) is wanted but needs explicit schema-driven mapping design — no inference magic (ADR-001).

> can that be variable so I could accept a file or it could accept as you could just accept a string or a variable or you know take a directory in in the workplace sort of figures it out from there
> — Tom, 2026-06-13

- **Links:** (none)
- **Notes:** Self-contained CLI work; strong first candidate for briefs-driven dispatch (RM-018).

### RM-004 — aion dev watch mode

- **Kind:** feature

Rebuild + repackage + hot-redeploy on file change for the authoring loop.

- **Links:** (none)

### RM-005 — Activity heartbeats

- **Kind:** feature

Coarse progress signal from long-running activities (agent steps run an hour-plus under ADR-003); confirmed wanted. Likely consumes the messaging bus's presence primitives if RM-020 lands first.

- **Links:** (none)
- **Notes:** Sequencing question vs RM-020 deliberately open.

### RM-006 — Worker task queues, routing, and affinity

- **Kind:** feature

Named task queues, routing keys, worker affinity — the locality story for filesystem-coupled activity families (same-host worktrees) and the scale-out story for everything else. Confirmed wanted.

- **Links:** (none)
- **Notes:** Candidate consumer of RM-020 (bus consumer groups + sticky routing).

### RM-007 — Dashboard per-run event timeline

- **Kind:** feature

Per-run event timeline view in aion-dashboard.

- **Links:** (none)

### RM-008 — Elixir SDK

- **Kind:** feature

BEAM-native polyglot authoring — the strategic counter to Temporal's client-runtime story; we never build client-side determinism cores.

- **Links:** (none)

### RM-009 — Declarative DSL + visual builder

- **Kind:** feature

On top of the typed SDK, not instead of it.

- **Links:** (none)

### RM-010 — WASM workflow runtime

- **Kind:** feature

Long-term polyglot path on beamr-wasm.

- **Links:** (none)
- **Notes:** Blocked by banked beamr items (WASM tail-park apply, WASM/JIT timer parity).

### RM-011 — Server robustness riders: log writer, banner count, connection logs

- **Kind:** fix

Broken-pipe-tolerant log writer (Ctrl-C with | jq spams tracing errors); startup banner reports workflow_package_count=0 despite persisted reloads; server-side worker connect/disconnect info logs.

- **Links:** (none)
- **Notes:** Three small disjoint fixes — the pilot wave for RM-018.

### RM-012 — Mint-or-resume agent sessions in dev handlers

- **Kind:** fix

Deterministic session ids exist; the dev handler should resume an existing norn session (sessions persist on disk, --resume exists) instead of always minting — making crash-resume reuse the SAME agent session.

- **Links:** (none)

### RM-013 — Worker SDK: unbounded-reconnect builder option

- **Kind:** fix

The SDK cannot express an unbounded reconnect budget; the stacked-dev worker works around it with usize::MAX. Builder should offer the explicit choice (ADR-001: the author chooses, the SDK doesn't default).

- **Links:** (none)

### RM-014 — Dispatch with no connected worker should park, not fail

- **Kind:** fix

Activity dispatch with no connected worker fails the run terminally; it should park until a worker serves the activity (consistent with ADR-003's unbounded-wait philosophy). Workers currently must start before aion start.

- **Links:** (none)
- **Notes:** Engine semantics — needs a short design pass (interaction with worker-loss delivery), not a direct brief.

### RM-018 — Briefs-driven self-hosted dev: aion work dispatched through aion

- **Kind:** process

Author the queued roadmap items as v2 clusters/briefs, dispatch them through the workflow family at the aion repo itself, review via meridian. The dogfood becomes the dev process. Pilot wave: RM-011's three disjoint fixes, run serially before fanning out.

> i'm kind of wondering if we can't do some of this work in Aon by you writing a bunch of input briefs like you did for that and we send out a bunch of non-agents and see how they go
> — Tom, 2026-06-13

- **Links:** (none)
- **Depends on:** RM-016, RM-017
- **Notes:** First real parallel batch (2026-06-13): RM-022/023/024 authored as parallel-runnable clusters. Parallel-safe waves — Wave 1: KIT-001 (foundation). Wave 2: KIT-002, KIT-003, OBS-001, OBS-002 (disjoint files, concurrent — the capacity stress test). Wave 3: WT-001..003 (serial, after aion_kit lands). Triggered as concurrent independent runs, since single-dispatch fan-out is serial pending RM-001 (parent-close).

### RM-019 — Release pipeline as an aion workflow (fifth template candidate)

- **Kind:** process

The 0.6.0 release by hand was workflow-shaped: ordered multi-crate publish, hour-class verify builds, a human ship-it approval signal, durable resume if it dies mid-wave. A workflow step could also mechanically sweep the version-bump ripple (scaffold-gate assertions, example/fixture lockfiles) that cost three gate runs.

> You know what this process would be a really good candidate for? an aion workflow. :D
> — Tom, 2026-06-13

- **Links:** (none)
- **Depends on:** RM-017

### RM-020 — Messaging bus on beamr (separate project; aion consumes)

- **Kind:** design

NATS-class bus built on beamr as its own project: the actor model as the wire protocol — durable mailboxes, native request-reply (a reply capability rides the message; no inbox/reply-channel ceremony), subscriptions as durable cursors, schema'd subjects, per-message delivery policy (required, ADR-001), presence/monitors first-class, credit-based backpressure, embedded-first. aion's heartbeats (RM-005) and queues/affinity (RM-006) become bus consumers instead of engine features.

> There are a whole bunch of kind of things that just kind of were a little bit annoying to me like you know that you had to like create a reply channel like that kind of stuff you know you couldn't just have things you know bounce and come back
> — Tom, 2026-06-13

- **Links:** (none)
- **Notes:** Separate repo when started; the design-system extraction (RM-016 note) rides with it.

### RM-021 — Workflow authoring experience: declare activities once, generate the rest

- **Kind:** design

Collapse new-family authoring to: schemas + one workflow module + one activities module. Everything that is today hand-mirrored gets generated — the Rust worker handlers (which must mirror locals invocation-for-invocation, i.e. a generator's job by definition), SERVED_ACTIVITIES, wire_compat pins, workflow.toml entries, locals/worker registration plumbing, and the hermetic-shim harness skeleton. Likely shape: `aion new workflow <name>` scaffolds inside an existing package; an activity declaration form (in Gleam or manifest) drives worker codegen the same way schemas already drive codec codegen. Evidence: in the brief-dev cluster, each activity exists in ~4 places that must agree byte-for-byte.

> I was sort of hoping for like a you write up the file and you know you might have like a file with some functions you might have a fight and then you just have sort of like remain work profile. It's just a couple of steps or something like that I don't know but like where can we reduce the authoring time and that kind of stuff?
> — Tom, 2026-06-13

- **Links:** (none)
- **Depends on:** RM-017
- **Notes:** Distinct from RM-003 (CLI input ergonomics) — this is family-authoring cost, not dispatch cost. Candidate to design right after the RM-011 pilot, informed by what the pilot proves painful. RM-022 (aion_kit) and RM-023 (dev-pipeline template) are the first concretisations of this; the generator work (worker handlers, manifests, harness skeleton) remains.

## Landed (1)

### RM-016 — Design system v2: ledgers, stage contracts, in-place enrichment

- **Kind:** process

docs/design-system/: roadmap + decision ledgers above the clusters, all document formats as schemas inside the aion codegen subset, stage contracts (scout/dev/review reports) as first-class schemas, briefs as living documents enriched in place, authoring/prompting/review guides, validation + rendering + coverage tooling.

> I would yeah I would love something that had yeah like like roadmap ledger like you say yeah roadmap decisions it all all those things like that and yeah that would be that would be really great and again like I'd really like you to apply your like your standards to it.
> — Tom, 2026-06-13

- **Links:** decisions ADR-007; commits c25ceeb8
- **Notes:** Extract to its own repo when RM-020 starts.
