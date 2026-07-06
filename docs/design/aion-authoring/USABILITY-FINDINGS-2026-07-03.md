# Usability Findings — 2026-07-03

**Author:** Wendy Testerberger (DX Auditor)
**Scope:** Aion ops console, authoring experience, CLI-to-deploy path
**Method:** 7 hands-on audit rounds (2026-06-07 through 2026-06-11) + fresh console/authoring survey (2026-07-03)
**Audience:** Exchange contract participants, Aion team

---

## Tier-0: Post-Mortem Inspection Bug (CONFIRMED)

When a workflow reaches terminal state (completed, failed, cancelled, timed out), the "Agent attempts" section in the workflow detail view is empty. The operator sees "No live attempts" — exactly when they need to inspect what happened.

### Root Cause

The workflow detail view has two independent data paths that don't share state:

1. **Timeline/swimlane** reads from `POST /workflows/describe` (durable event history) via `useWorkflowHistory`. This works correctly for terminal runs — the full event log is returned, including `ActivityScheduled`, `ActivityStarted`, `ActivityCompleted`, and `ActivityFailed` events.

2. **Attempts panel** reads from `POST /workflows/attempts` (live worker enumeration) via `useActivityAttempts`. This endpoint returns only attempts with a live owning worker. For terminal workflows all workers have exited, so the response is `{ attempts: [] }`. There is no fallback to event history.

### Render Chain

1. `WorkflowDetailView.tsx:187` unconditionally renders `<AttemptTranscriptView>` with no terminal-state awareness.
2. `AttemptTranscriptView.tsx:56-60` calls `useActivityAttempts({ workflowId, namespace })` — no status gating.
3. `useActivityAttempts.ts:99-100` calls `apiClient.listAttempts()` → `POST /workflows/attempts`.
4. Server returns `[]` (correct server behavior — it answers the question asked).
5. `AttemptTranscriptView.tsx:146-151` sees `attempts.length === 0` → renders "No live attempts" empty state.
6. Because `selected` is null, neither `TranscriptPanel` nor `InterventionControls` render (lines 106-119). The entire bottom section is blank.

### Recommended Fix

**Option A (recommended):** Derive completed attempts from the event history events that the timeline already has on the client. The `ActivityScheduled`/`Started`/`Completed`/`Failed` events contain everything needed to reconstruct the attempt list. Operators want to see what happened, and the data is already fetched.

**Option B:** Have the server's `/workflows/attempts` endpoint return historical attempts for terminal workflows.

**Option C (minimum):** Pass `isTerminal` into `AttemptTranscriptView` and show a meaningful post-mortem UX rather than "No live attempts."

### Key Files

- `apps/aion-ops-console/src/features/workflow-detail/swimlane/WorkflowDetailView.tsx:187`
- `apps/aion-ops-console/src/features/transcript/components/AttemptTranscriptView.tsx:56-60, 146-151`
- `apps/aion-ops-console/src/features/transcript/hooks/useActivityAttempts.ts:99-100`
- `apps/aion-ops-console/src/lib/api/client.ts:458-467` (JSDoc confirming live-only semantics)

---

## CLI-to-Deploy Path (Rounds 1-7 Summary)

The core `aion package` → `aion server` → `aion run` path works end-to-end. After fixes landed across rounds 1-5, the hello-world example completed successfully for the first time. The embedded engine path is solid.

### Transport-Layer Issue (CONFIRMED, still open)

Server-to-worker gRPC dispatch has ~30-60 second latency on the distributed path, causing activity timeouts in live demos. The embedded engine path (in-process activities) works perfectly. This is the single biggest blocker for anyone evaluating the distributed worker model. For exchange audiences, this will be the first thing that bites them if they follow the getting-started guide with a remote worker.

---

## Ops Console Findings

### Confirmed Live

**Start-form workflow type is free text.** `StartWorkflowForm.tsx:106` is a plain `<input>`, not a combobox against deployed workflow types. After deploying a package, the operator must remember the exact type string to start a workflow. For an outside adopter who just deployed their first package, this is a "why is this hard" moment. The fix is a combobox that queries deployed types.

### Fixed Since June 11 Snapshot

**Fonts.** DM Sans and JetBrains Mono are now self-hosted with woff2 bundled in the build output. No longer falling back to system fonts.

**`--surface-elevated` token.** Now defined in both light and dark themes (`index.css:61/139`). The transparent-container issue on the swimlane is resolved.

### Needs Verification Against Current Main

**WebSocket resubscribe storm.** Effect dependencies in `useEventSubscription` were causing unsubscribe/resubscribe on every incoming event. Rated Critical in `COMPLETE-SCOPE.md` (item R4). May have been addressed since my audit snapshot — needs a quick check.

**Swimlane is sequence-based, not time-proportional.** A sub-second activity renders the same width as a 10-minute agent step. For the assistant workflow especially, this makes the timeline useless as a diagnostic tool. The design doc (`OPS-CONSOLE-UX.md`) calls for time-proportional rendering — needs verification on whether this has landed.

**Assistant draft persistence.** The assistant panel's chat input does not persist drafts across navigation. If you're mid-message and click to another tab, it's gone. Needs verification.

---

## Authoring Experience Friction

### First Contact is the Worst Path

`aion new` scaffolds are static templates not wired to the `aion generate` code generator. A new developer following the scaffold gets the fully hand-written path: manual codecs, manual activity wrappers, manual worker stubs. They don't discover the declare-once model (`manifest()` + `aion generate`) until they read deeper documentation or stumble across it in the order-saga example.

The fix: wire `aion new` scaffolds to emit a `manifest()` function and include `aion generate` as a build step, so the first project a developer creates uses the best available tooling.

### The Run Ceremony

Every workflow hand-writes ~27 lines of identical `run(raw_input: Dynamic)` boilerplate (decode input, handle errors, wire up signal handlers). `workflow.entrypoint` exists as the adapter that eliminates this, but the scaffolds and most examples don't use it yet. A new developer copies the pattern from examples and inherits the ceremony.

### The Codec Tax

Gleam has no derive macros, so every type that crosses the engine boundary needs a hand-written JSON encoder and decoder. For real workflows with many activity types, this is thousands of lines. `aion generate` mitigates this effectively for the declare-once model, but this reinforces the importance of wiring `aion new` to the generator — the codec tax is only painful when the generator isn't in the loop.

### Same Shape Described Three Times

A boundary type exists as a Gleam type, a `schemas/*.json` file, and a Rust/Python/TypeScript worker struct. The generator closes this gap, but only if you know it exists and opt in.

---

## Three Priorities for the Exchange Audience

In order of impact on first-contact experience:

1. **Fix post-mortem inspection** (Tier-0 bug above). Outside adopters will demo a workflow, let it complete or fail, then try to inspect what happened. An empty attempts panel reads as "broken" — it's the worst possible first impression for an ops console.

2. **Wire `aion new` to the generator** so first contact is the best available path. The declare-once model with `aion generate` is genuinely good DX — it just needs to be the default, not a discovery.

3. **Make the start-form type-aware.** A combobox against deployed types eliminates the "what do I type here" moment that follows every first deploy.

These three close the biggest "why is this hard" moments in the current experience. The engine underneath is production-quality (944 tests across 5 languages, all passing, clean clippy, recovery works, all 5 load-bearing invariants hold). The gap is between the engine's quality and the surface that presents it.
