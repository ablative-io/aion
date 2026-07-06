---
type: review
title: Aion authoring experience + ops console — team review & recommendations
date: 2026-07-03
reviewers: Max Power (authoring DX/model), Charge (in-console assistant), Lemmy (ops console vs VISION), synthesis by Frodo
pending: Wendy Testerberger (usability / user-tester findings) — to be appended
context: Tom asked for the team's grounded thoughts, weighted toward the authoring experience, as we move to get the team (and an external exchange-contract community) using Aion.
---

# Authoring experience + ops console — review & recommendations

## Headline

This is **real, disciplined, trustworthy work — not a scaffold.** The core diagnosis
loop (swimlane → scrub → reopen), the omni-palette, the Actions surface, the token
system, the durable-assistant design, and determinism-as-a-static-gate are either
category-leading or solidly good. The scaffold-audit problems from the last round are
**resolved** (the swimlane now handles all 29 event variants behind a compile-time
exhaustiveness guard). The gap between what's shipped and "the best authoring experience
in the category" is **a small number of concrete, well-bounded fixes — not a rethink.**

The gaps cluster into four themes. Three reviewers hit the first one independently.

---

## Theme 1 — The authoring inner loop isn't closed end-to-end (the adoption blocker)

This is *the* theme for "get the team (and a community) using it." The payoff moment —
author → deploy → run → watch — is the clunkiest part of the whole experience.

- **The dev loop is an onboarding cliff** (Max). To run one workflow you flip four
  dark-by-default env switches (`AION_DEV_ENABLED`, `AION_DEPLOY_ENABLED` + two
  max-bytes), run two terminals, then **start a run by hand-curling `/dev/runs` and
  manually typing a WebSocket `subscribe` frame**. ADR-001 (no invented defaults) is
  right for production but turns first-run into a wall.
- **The author→deploy loop is open** (Charge). The in-console assistant authors a real
  package into a workspace on disk, but the session view shows only transcript + chat —
  the author must hunt `~/.aion/clones/<run_id>/repo` and deploy by hand. Worse, the
  server's *real* authoring endpoint — `compile_and_load` (inline typecheck + hot-load,
  `crates/aion-server/src/authoring/handlers.rs`) — **is not wired to the assistant or
  the console at all.** Two disconnected authoring paths that never meet.
- **The workflow↔worker split still exists** (Max). The dispatch seam
  (`crates/aion/src/runtime/nif_activity_dispatch.rs`) unconditionally routes every
  activity to a remote worker; `tier` never crosses it. So the common case still needs a
  separate worker binary — the exact thing DBOS/Inngest/Restate have collapsed. The
  in-VM primitive and the stored Gleam body already exist; only the seam is unconditional.

## Theme 2 — Decided cheap wins that should just ship (independent of the syntax fork)

The authoring-model discussion already settled these; they're safe, additive, high-leverage.

1. **`workflow.entrypoint` — hide the `run`/`Dynamic` shim.** DESIGN.md says "the author
   should never see `run` or `Dynamic`," yet every template still ships a visible ~15-line
   `run(raw_input: Dynamic)` adapter. It's the single biggest gap between the stated ideal
   and the shipped scaffold. Blocked only on the aion_flow 0.5.0 pin.
2. **Wire the in-VM tier through the dispatch seam** (Theme 1) — the common workflow then
   needs *no* separate worker; remote Rust/Python becomes the opt-in escape hatch.
3. **Put `aion new agent` on the generated path.** Today it's a static ~300-line template
   (with ~60 lines of hand-written error-taxonomy mappers), so a *new* user gets the worst,
   most-hand-written path. Move the mappers into the SDK; scaffold intent, not plumbing.

## Theme 3 — The console's VISION gap is two well-bounded fixes

Lemmy's scorecard: six core concepts implemented (swimlane, scrubber, reopen diff, event
search, palette all real; triage partial-by-design with cluster classes honestly gated as
"awaiting server support"). The five professional-console disciplines are where the gap is:

- **URL-as-state (ADR-015) — the headline gap. PARTIAL.** Selected bar and scrub seq are
  component `useState`, namespace is React context, search query isn't in the URL. So the
  discipline VISION calls "the single strongest professional-software tell" — paste a Slack
  link, land on the same bar at the same scrub — **does not work yet.** Everything already
  computes from those values; it's a lift into the router, not a rethink. **Highest leverage.**
- **Swimlane virtualization + a measured budget (ADR-018). PARTIAL.** Transcript virtualizes
  and the list paginates, but the swimlane — the explicit 10k-event target — renders every
  bar unwindowed, and there are no measured budgets anywhere. Correctness is done; perf is not.
- Smaller: promote provenance (ADR-016) from the failover surface to a **global chrome line**
  (node + last-applied seq on every view); add **in-view keyboard navigation** (bar/incident/
  result traversal) to finish ADR-017's operation layer; give the **calm state (ADR-019)** a
  designed always-on cluster-heartbeat home.

Design language is genuinely good and **not** over-engineered — the raw-colour guard test
(bans hex/rgb/oklch incl. `var()` fallbacks) is real discipline. One narrow decoration risk
(`animated-background.tsx` is the "constellation screensaver" VISION permits only as an
after-nicety; the `Entity` morph is elaborate) but it's on the right side of the hand-plane line.

## Theme 4 — The one genuinely-open call: the authoring syntax flavour

Everything else in the authoring model has converged (types-first verified feasible via
`gleam export package-interface`; the `.aion` file becomes the canonical model; file/dir
inputs via haematite content-addressing). The open call is the **syntax flavour**, which has
moved YAML-DSL → TypeScript-subset (F) → **step-document (G)**.

**Position (Frodo + Max, aligned):**
- **The decisive lens is that AI agents will be the primary authors.** That favours a
  *data* surface (schema-validatable, safe under constrained decoding) over a code-lookalike
  (which gives LLMs plausible-but-wrong priors). That points at **G**.
- **G is the right primary surface — but only if its expression micro-grammar stays
  ruthlessly minimal**, with `aion check` giving compiler-grade diagnostics. The moment the
  `do:`/`when:`/`finish:` expressions grow, you've rebuilt the language-in-YAML footgun G was
  meant to avoid. The discipline that keeps it safe *and* keeps determinism by-construction:
  **the only verbs are call-an-action / bind / branch / fan-out / wait; anything computational
  lives in an action.**
- **Real synergy with the console:** G's per-step `about:` prose feeds the live ops-console
  narration of a running workflow (the NOI fit). The surface Tom's leaning toward and the
  console he's built reinforce each other.
- **Keep F (TypeScript) as a later code-first *view*** via the canonical model, for engineers
  who want it. "Multiple languages to work from," one source of truth.
- **Reframe the claim:** stop saying "no DSL." The claim actually being upheld is **"no second
  *source of truth*."** G and Gleam are both lossless views of the `.aion` graph. Different
  claim; only the second one is true, and it's the stronger one.

---

## Concrete bugs / polish worth flagging (matter for external users)

- **Hardcoded host path leakage.** `examples/assistant/resources/ENVIRONMENT.md` bakes in
  `/Users/tom/Developer/ablative/aion`, embedded at compile time into *every* workspace and
  declared PRIMARY over the repo. It will mislead any user outside Tom's machine — a real bug
  for the exchange community.
- **Skill-doc versioning is weak** — docs are `include_str!`'d at worker-build time, so an
  authoritative-but-stale doc drifts silently from the deployed SDK. Stamp them with an
  SDK/commit version.
- **`assistant_status {phase,round}` isn't surfaced** — the slow provisioning clone looks
  identical to a transient "Connecting" gap. Cheap fix, kills the worst ambiguity.
- **One textbox, two meanings** — a message typed while a round is *live* is injected as an
  interrupt, not queued as the next round. Timing-dependent; wants explicit confirm/queue.

---

## Recommended sequencing

**Tier 0 — unblock adoption (days, do first):** ship `workflow.entrypoint` + delete the
visible `run` shim; add a one-command `aion run <type> --input … --watch` (replacing the
curl + hand-typed WS frame); strip the hardcoded path + version the skill docs. These remove
the friction a new external user hits first.

**Tier 1 — close the loop:** wire the in-VM tier (no separate worker for the common case);
close the author→deploy loop in the console (surface the workspace + one-click deploy through
the existing `compile_and_load` endpoint).

**Tier 2 — console to full VISION:** URL-as-state (ADR-015); swimlane virtualization + a
measured budget (ADR-018); global provenance line; in-view keyboard nav; calm-state home.

**The decision to make:** the syntax fork — G as primary (with the minimal-grammar
discipline), F as a later view, "no second source of truth" as the honest framing.

Net: the bones are category-leading. The path to "best in class" is mostly *finish the
decided cheap wins* and *close the inner loop* — engineering, not architecture — plus the one
real design decision on the syntax surface.

---

## Usability findings — Wendy Testerberger (user-tester, rounds 1-7 + fresh survey)

The user's-eye layer. Verified items are annotated (her console snapshot was June 11, so some
cosmetic items have since been fixed — verification against current main matters).

**Confirmed live (structural — highest value):**
- **Post-mortem inspection is broken — CONFIRMED with root cause (Wendy repro).** The
  attempts/transcript panel shows nothing for a *finished* run — exactly when an operator needs
  to inspect after a crash (the first thing an exchange audience does after a demo failure).
  **Her #1 priority.** Root cause: two independent data paths. The attempts panel
  (`AttemptTranscriptView`) enumerates *live* attempts via `POST /workflows/attempts`, which
  only returns attempts with a live owning worker; for a terminal workflow all workers have
  exited, so the server honestly returns `{ attempts: [] }` and the panel renders "No live
  attempts" with the whole transcript/intervention section blank. The timeline/swimlane works
  because it reads durable history via `POST /workflows/describe` — it never touches the
  attempts path. Chain: `WorkflowDetailView.tsx:187` renders `<AttemptTranscriptView>` with no
  terminal-awareness → `useActivityAttempts.ts:99-100` hits the live endpoint with no status
  gating → `AttemptTranscriptView.tsx:146-151` shows the empty state.
  **Fix (recommended, Option A):** derive completed attempts from the event history the timeline
  already has when the workflow is terminal — operators want to see what happened, and the data
  is already on the client. (B: server returns historical attempts for terminal runs; C minimum:
  pass `isTerminal` and show meaningful UX.) Clean, bounded **Tier-0 fix**.
- **Start-form workflow type is free text** — VERIFIED live (`StartWorkflowForm.tsx:106` is a
  plain `<input>`, no combobox/datalist against deployed types). An adopter deploys a package
  then has to remember the exact type string to start it. The "why does it do that" moment.
- **~30-60s server→worker gRPC dispatch latency** on the distributed path, causing activity
  timeouts in live demos. Nobody else flagged this — it's the thing that bites anyone trying
  the *distributed* path first. (The embedded-engine path is solid.) Worth a focused perf look.
- **Swimlane is sequence-based, not time-proportional** — a sub-second activity renders the
  same width as a 10-minute agent step, which makes the assistant timeline near-useless as a
  diagnostic. This is by-design per VISION (seq-ranked), but Wendy's usability point is real:
  for diagnosis you want time-proportional (or a toggle). A genuine design call the code reviews
  didn't surface — it takes actually *using* it. Worth adding to the swimlane spec.

**Fixed since her June-11 snapshot (VERIFIED against current main):**
- ~~Fonts never load / no `@font-face`~~ — fonts are self-hosted and bundled now (woff2 in
  build output; @fontsource injects @font-face via the bundler). Loads correctly.
- ~~`--surface-elevated` referenced but undefined → transparent containers~~ — defined in both
  themes (`index.css:61` dark, `:139` light). Resolved.

**Needs verification against current main (from her survey; may be pre-fix):**
- **WebSocket resubscribe storm** — effect deps in `useEventSubscription` causing unsub/resub
  per event (rated Critical in the earlier scope doc). Relates to the live-feed issues from the
  prior scaffold audit; verify whether the current live-feed refactor resolved it.
- **Assistant draft persistence across navigation not wired** — note this contradicts the
  console review (which saw a zustand/module-level draft store, commit 7bd93122); reconcile
  whether it's forms-generally (wired) vs the assistant panel specifically (maybe not).
- **Token vocabulary split** (bespoke `--text-*`/`--surface-*` + shadcn `--foreground`/`--card`
  coexist) — partly cosmetic; the specific undefined-token bug is fixed, but the two-vocabulary
  coexistence is worth a deliberate reconcile.

**Corroborates the team review (authoring):** the run ceremony (~27 lines, `workflow.entrypoint`
exists but scaffolds don't use it), the codec tax, the same-shape-3× problem, and — critically —
`aion new` scaffolds being static templates *not wired to the generator*, so a new user's first
build is the fully-hand-written worst path. This is the same finding as Max Power's, from the
user side: **first contact should be the best path, not the worst.**

**Wendy's three priorities for the exchange audience:** (1) fix post-mortem inspection,
(2) wire `aion new` to the generator so first contact is the best path, (3) make the start-form
type-aware. Those three close the biggest "why is this hard" moments.

**On the exchange contract:** Wendy is in as the usability/user voice (confirmed).
