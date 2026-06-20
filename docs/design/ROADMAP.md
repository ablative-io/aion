# aion — Roadmap

_Updated: 2026-06-21_

## Briefed (11)

### RM-009 — Bidirectional visual canvas: code↔diagram projection with live overlay

- **Kind:** feature

On top of the typed SDK, not instead of it: because a workflow is a typed function over a small known vocabulary (run / spawn / receive / all / race / sleep), render it to a graph automatically, overlay a live run's progress on the same graph, and allow bounded structural round-trip — a projection of the typed source, never a second authoring surface (ADR-014). Reframes the original 'declarative DSL + visual builder' idea away from a separate DSL and toward a generated, always-in-sync canvas that doubles as the monitoring view.

- **Links:** cluster `aion-authoring`; decisions ADR-014; briefs WA-005
- **Notes:** Briefed as WA-005. The graph model is generated from the workflow's extracted primitive structure and never the authoritative artifact (CN6). Scope (2026-06-21 frontend call): this cluster delivers the graph model + per-node correlation identity + Gleam regeneration; the rendered canvas UI and live overlay are deferred to the dashboard family (RM-007).

### RM-017 — brief_dev + dispatch workflow family

- **Kind:** feature

The all-norn inner dev pipeline as an aion workflow (scout → dev → firsthand gate with fix-until-clean resume → adversarial review → harden → re-gate, progressive in-place enrichment, deterministic sessions), composed under stacked_dev, plus a dispatcher workflow that selects briefed work from roadmap.json, orders by dependencies, fans out children with explicit parent-close (ADR-004), serializes lands.

> I would love it if you could write a workflow that that were not just right I mean write a workflow that sort of handles the pattern so goes through like and we try to do it all as much as we could with norn via the workflow.
> — Tom, 2026-06-13

- **Links:** cluster `brief-dev`; decisions ADR-008, ADR-009; briefs BD-001, BD-002, BD-003, BD-004, BD-005, BD-006, BD-007
- **Depends on:** RM-016
- **Notes:** Briefed 2026-06-13, coverage clean. ADR-008/ADR-009 DECIDED by Tom same day — blocked_by on BD-003/004/005 cleared. BD-003..005 must land as ONE wave (the family doesn't compile between BD-003's module deletion and BD-005's rewire). Implementation in flight.

### RM-021 — Workflow authoring experience: declare activities once, generate the rest

- **Kind:** design

Collapse new-family authoring to: schemas + one workflow module + one activities module. Everything that is today hand-mirrored gets generated — the Rust worker handlers (which must mirror locals invocation-for-invocation, i.e. a generator's job by definition), SERVED_ACTIVITIES, wire_compat pins, workflow.toml entries, locals/worker registration plumbing, and the hermetic-shim harness skeleton. Likely shape: `aion new workflow <name>` scaffolds inside an existing package; an activity declaration form (in Gleam or manifest) drives worker codegen the same way schemas already drive codec codegen. Evidence: in the brief-dev cluster, each activity exists in ~4 places that must agree byte-for-byte.

> I was sort of hoping for like a you write up the file and you know you might have like a file with some functions you might have a fight and then you just have sort of like remain work profile. It's just a couple of steps or something like that I don't know but like where can we reduce the authoring time and that kind of stuff?
> — Tom, 2026-06-13

- **Links:** cluster `aion-authoring`; decisions ADR-014; briefs WA-001
- **Depends on:** RM-017
- **Notes:** Briefed 2026-06-21 as WA-001 (declare-once codegen) — Layer 1 keystone of aion-authoring. Bound by ADR-014 (typed module is the single source of truth; no separate DSL), ratified 2026-06-21. Designs the generator work (worker handlers, codecs, manifests, wire goldens, test skeletons). Distinct from RM-003 (CLI input ergonomics).

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

### RM-025 — aion dev: instant authoring loop + local dev server with production parity

- **Kind:** feature

`aion dev` watches a package and rebuilds, repackages, and hot-reloads on save, and serves a local dev UI with production parity — trigger a run, watch its events stream live, mock an activity, replay a failed run — over the same engine, store, and event stream production uses. The single biggest time-to-value lever the durable-execution field has converged on (Inngest, Restate, Temporal all ship one), and Aion gets the hard pieces nearly free: hot code loading is the reload, content-hash versioning makes concurrent versions safe, and the WebSocket event firehose is the live view. Subsumes the RM-004 watch slice.

> either like a DSL or something like that to generate code to scaffold ... an experience that would qualify for best in class
> — Tom, 2026-06-21

- **Links:** cluster `aion-authoring`; decisions ADR-014; briefs WA-002
- **Depends on:** RM-021
- **Notes:** Briefed as WA-002. Folds in RM-004 (watch mode). CN4: no mock-only path diverges from production; activity mocking is opt-in per run. Scope (2026-06-21 frontend call): the dev-server endpoints and the hot-reload loop are in scope; the rendered web client is deferred to the dashboard family (RM-007).

### RM-026 — Server-as-compiler authoring loop (aion-toolchain)

- **Kind:** feature

Build the designed-but-absent aion-toolchain crate and aion-server authoring endpoints: submit Gleam source, the server shells out to the gleam binary, type-checks, returns errors inline, packages a .aion, and hot-loads it — an authoring REPL against a running engine with no local toolchain. An asset only an engine that owns a hot-loading VM can offer (Temporal structurally cannot), and the foundation for a hosted web playground. Optional and gated on --gleam-path; without it the server deploys pre-built .aion only.

- **Links:** cluster `aion-authoring`; briefs WA-003
- **Notes:** Briefed as WA-003. CN7: aion-toolchain shells out to the gleam binary, never embeds it; aion and aion-server carry no compiler dependency without --gleam-path.

### RM-027 — Time-travel debugger over the event-store oplog

- **Kind:** feature

A scrubber in the dashboard over a run's history: step event-by-event, see workflow-visible state and the recorded now()/random() at each step, and on a NonDeterminismError point at the exact divergent command (expected vs found) the resolver already computes — plus a what-if re-run from any event via the existing test harness. Golem and Temporal make time-travel a flagship feature; Aion gets it nearly free because the event store is already a complete oplog, replay reconstructs exact state, and the determinism mismatch is already computed. The data exists; the build is the per-event state projection plus the UI.

> Feel free to get creative and be provide innovative solutions
> — Tom, 2026-06-21

- **Links:** cluster `aion-authoring`; decisions ADR-007; briefs WA-004
- **Notes:** Briefed as WA-004. Scope (2026-06-21 frontend call): delivers the engine state projection + determinism diff + the `aion inspect` stepping/what-if data surface; the rendered dashboard time-travel scrubber UI is deferred to RM-007 (which also owns the per-run event timeline). CN5: reads existing history and replay, no parallel debug log.

### RM-028 — Agentic-first authoring: aion new agent scaffold

- **Kind:** feature

`aion new agent` scaffolds a durable agent loop (scout -> act -> verify -> signal-gated review) parameterised by prompts + schemas + gate, generalising the dogfooded stacked-dev shape so a new agentic family is configuration, not bespoke code. Human-in-the-loop is just workflow.receive with a timeout — the headline feature of LangGraph-class tools is already an Aion primitive, and durable suspend-for-weeks is already free. Where the market is sprinting (Golem's agentic refocus), and where Aion's first consumer already lives.

- **Links:** cluster `aion-authoring`; briefs WA-006
- **Depends on:** RM-021, RM-023
- **Notes:** Briefed as WA-006. The human approval pause is a workflow.receive with a timeout (C27), not a bespoke poll. Depends on WA-001 codegen and the RM-023 dev-pipeline template.

### RM-029 — Determinism linter + generated test scaffolds + input skeletons

- **Kind:** feature

`aion check --deterministic` is a static lint that flags any wall-clock or entropy call reachable from workflow code, turning the determinism boundary into a CI gate that complements the SDK's type-level structural block — a provable-determinism guarantee no competitor offers. Alongside it, `aion generate` emits an aion/testing skeleton per workflow (each activity mocked, a clock advance per timer, a replay-determinism assertion) and `aion input <type>` emits a valid input skeleton from the workflow's input type, so testing and triggering start from a scaffold, not a blank file.

- **Links:** cluster `aion-authoring`; briefs WA-007
- **Depends on:** RM-021
- **Notes:** Briefed as WA-007. Relates to RM-003: the `aion input` skeleton is generated from the workflow input type here; RM-003's client-side validation and polymorphic --input remain its own dispatch-time work.

## Designed (2)

### RM-001 — Implement parent-close policy

- **Kind:** feature

Required per-spawn RequestCancel | Terminate | Abandon on child.spawn / spawn_and_wait; propagation on all parent terminals, recursive, with recovery re-arming pending propagations. SDK + docs + template updates ride along.

- **Links:** cluster `parent-close`; decisions ADR-001, ADR-002, ADR-004; briefs PC-001, PC-002, PC-003
- **Notes:** Cluster designed (parent-close): PC-001 type foundation, PC-002 engine propagation + recovery re-arm, PC-003 SDK + call sites + e2e test. Serial: PC-001 first, then PC-002 and PC-003 (both depend on PC-001).

### RM-015 — Multi-reviewer verdict coordinator

- **Kind:** feature

Reviewers vote via meridian review complete; the Meridian coordinator applies quorum and fires the single review_verdict signal. aion-side contract already live-proven; the work is Meridian-side plus the branch→workflow-id mapping seam at review-request time.

- **Links:** decisions ADR-006
- **Notes:** Implementation lives in the yggdrasil/Meridian repo and rides their re-pin to published aion 0.6.0 + hex aion_flow 0.4.0 (pins currently 88 commits behind at rev 489be454).

## Idea (15)

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

## Landed (1)

### RM-016 — Design system v2: ledgers, stage contracts, in-place enrichment

- **Kind:** process

docs/design-system/: roadmap + decision ledgers above the clusters, all document formats as schemas inside the aion codegen subset, stage contracts (scout/dev/review reports) as first-class schemas, briefs as living documents enriched in place, authoring/prompting/review guides, validation + rendering + coverage tooling.

> I would yeah I would love something that had yeah like like roadmap ledger like you say yeah roadmap decisions it all all those things like that and yeah that would be that would be really great and again like I'd really like you to apply your like your standards to it.
> — Tom, 2026-06-13

- **Links:** decisions ADR-007; commits c25ceeb8
- **Notes:** Extract to its own repo when RM-020 starts.
