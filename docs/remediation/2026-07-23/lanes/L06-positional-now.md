# L06 — Positional `now()`

**Findings:** F-1 (major — the review's sharpest finding; invariant 2 / DESIGN CO8 violation) · **Risk:** **deep_tear** · **Depends on:** — · **Status:** ready

The one confirmed determinism-boundary violation in the engine: workflow-visible `now()` serves a non-positional timestamp and silently corrupts `now()`-as-data across recovery. **This lane blocks on Vesper Lynd's recorded APPROVE before merge.**

## dev_brief input

```json
{
  "brief": {
    "id": "rem-l06-positional-now",
    "title": "Serve positional now() from the cursor position (F-1)",
    "objective": "Workflow-visible now() must be positional: at any point in workflow execution, now() serves the recorded_at of the last history event APPLIED at the current replay/execution position — so a given call site observes the same value live, under replay, and under full-history re-execution recovery. Today it does not: now_from_context (crates/aion/src/runtime/nif_determinism.rs, around line 216) returns context.last_recorded_at(), a field NifContext sets ONCE at construction from history.last() over the full run segment (nif_context.rs, around lines 117-118); nif_timer.rs (around lines 341-346) does the same. Note the confirmed behavior is worse than 'diverges on replay': even within one live segment, every now() call returns the construction-time TAIL timestamp, not the position-time value. Meanwhile the durability layer already models now() positionally — DeterminismContext::advance_to_recorded_at advances per applied event, and replay_inspect.rs (around lines 145-148) documents per-step now = event.recorded_at() as 'exactly the value the production now() NIF serves' (currently true only for the final step). Fix: thread the positional timestamp — the value the cursor/DeterminismContext already tracks per applied event — into the NIF path, replacing the construction-time snapshot in both nif_determinism.rs and nif_timer.rs. For a now() call at a point where new history is being appended live (past the replayed prefix), the served value must be the recorded_at of the most recently APPLIED/recorded event at that moment, consistent between the live run and any later re-execution of the same history. replay_inspect's documented semantics are the specification; after the fix its claim must be simply true. Required tests: (1) a replay-divergence regression test — a workflow calling now() early (before a blocking point), with later events (e.g. a signal) extending history; assert the value observed live equals the value observed under recovery re-execution (this test MUST fail against the current code); (2) a positional-progression test — successive now() calls across yield points observe non-decreasing, position-correct timestamps matching replay_inspect's per-step values; (3) timer-path equivalents for the nif_timer.rs site.",
    "context": "Source review: docs/REVIEW-23-07.md F-1, adversarially verified AND independently re-confirmed at current main bytes by Vesper (last_recorded_at is a construction-time snapshot). Compatibility note for the dev report: histories recorded under the old semantics already diverge between live and recovery observation (that is the bug), so serving positional values to in-flight old workflows does not break a stability that existed — but state this analysis explicitly, and consider whether any shipped code path RELIES on tail-timestamp semantics (grep callers of last_recorded_at and the now NIF). The determinism boundary is the engine's core invariant — treat every change here as production-critical. DESIGN CO8 and docs/design determinism material are the contract; read them before coding.",
    "pointers": [
      "docs/REVIEW-23-07.md (F-1)",
      "crates/aion/src/runtime/nif_determinism.rs (now_from_context)",
      "crates/aion/src/runtime/nif_context.rs (last_recorded_at construction)",
      "crates/aion/src/runtime/nif_timer.rs (the parallel site, ~341-346)",
      "crates/aion/src/durability (DeterminismContext::advance_to_recorded_at, HistoryCursor)",
      "replay_inspect.rs (~145-148 — the intended semantics, documented)",
      "docs/design determinism contract (CO8)"
    ],
    "scope_in": [
      "crates/aion/src/runtime/nif_determinism.rs",
      "crates/aion/src/runtime/nif_context.rs",
      "crates/aion/src/runtime/nif_timer.rs",
      "Minimal cursor/DeterminismContext surface needed to expose the positional value to the NIF path",
      "Tests: runtime determinism suites, replay/recovery e2e"
    ],
    "scope_out": [
      "NO changes to random() (correctly seeded; not part of this finding)",
      "NO changes to event schemas, recorded history format, or the store layer",
      "NO changes to replay_inspect's documented semantics — the code moves to the doc, not the doc to the code",
      "NO changes to HistoryCursor's interleaving-aware application logic beyond exposing the positional timestamp",
      "NO reorganization of any touched file",
      "Workspace laws: no unwrap/expect/panic (tests included), no #[allow], typed errors, files ≤500 lines"
    ],
    "acceptance": [
      "CHARACTERIZE BEFORE BUILDING: the first commit on the lane is a characterization suite pinning the CURRENT (buggy) behavior at the base bytes — probe the exact divergence, record the observed values as test assertions, and state predictions in the dev report BEFORE any fix code exists; the fix then flips those pinned assertions to the correct semantics in a reviewable diff",
      "The replay-divergence regression test exists, is demonstrated to FAIL against the pre-fix code (evidence in the dev report: the failing run output), and passes after the fix",
      "Successive now() calls observe position-correct, non-decreasing values equal to replay_inspect's per-step now at every step (test evidence)",
      "The nif_timer.rs site serves the same positional value as the nif_determinism.rs site (test evidence)",
      "replay_inspect's 'exactly the value the production now() NIF serves' claim is true at every step, not only the final one",
      "Caller analysis for last_recorded_at / now-NIF reliance on tail semantics is in the dev report with a conclusion per caller",
      "BLAST-RADIUS STATEMENT: the dev report explicitly states the fix's effect on existing recorded histories and in-flight runs (replay-equals-live is the engine's core promise and downstream acceptance suites lean on it); if the analysis concludes any migration or history-versioning question exists, that is a STOP — the lane halts as an escalation naming Vesper Lynd, Waffles the Terrible, and Tom (Waffles holds the technical ruling on migration forks), never a unilateral call",
      "Full existing determinism, replay, recovery, and timer suites green"
    ],
    "notes": "DEEP TEAR LANE: after gates pass, the coordinator does NOT merge — it escalates to Vesper Lynd with the branch, the dev report, the failing-then-passing regression evidence, and every lens verdict, and blocks on her recorded verdict. The demonstrated-red-then-green requirement on the regression test is non-negotiable — a regression test that never failed proves nothing."
  },
  "config": {
    "repo_root": "{REPO_ROOT}",
    "base_branch": "main",
    "gates": [
      {"name": "fmt", "argv": ["cargo", "fmt", "--all"]},
      {"name": "clippy", "argv": ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]},
      {"name": "test", "argv": ["cargo", "test", "--workspace"]}
    ],
    "verify_gates": [
      {"name": "clippy", "argv": ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]},
      {"name": "test", "argv": ["cargo", "test", "--workspace"]}
    ]
  }
}
```
