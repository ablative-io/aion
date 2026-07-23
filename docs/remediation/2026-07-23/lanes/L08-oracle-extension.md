# L08 — Conformance oracle extension

**Findings:** F-9 (major), m-2, F-8 (contract half) · **Risk:** small · **Depends on:** **L07** · **Status:** ready

Pins into the shared store-conformance oracle the safety-critical behaviors currently guarded only by per-backend private tests — including the ones whose absence let F-3 go uncaught.

## dev_brief input

```json
{
  "brief": {
    "id": "rem-l08-oracle-extension",
    "title": "Extend the shared store conformance oracle: Paused/list_paused, non-contiguous append, outbox lifecycle + contention (F-9/m-2/F-8)",
    "objective": "The shared conformance suite (crates/aion-store/src/conformance.rs) omits behaviors every backend must implement identically. Extend it with four scenario families, each run against BOTH shipped backends (memory-reference semantics define expected outcomes): (1) Paused/list_paused — currently zero 'paused' hits in the suite despite list_paused being a required trait method tied to kill-9 pause recovery: scenarios for pause → list_paused returns the run; resume/cancel → it leaves the set; list_active excludes Paused (the F-8 behavioral contract: list_active filters Running ONLY — pin the Running-only semantics both shipped backends implement, and fix the list_active trait doc comment to say so if L01 has not already landed that wording). (2) Non-contiguous append rejection — no scenario appends a mis-sequenced batch though the reference store rejects them (memory.rs ~303-311): scenarios for a gapped batch, an overlapping/stale batch, and the exact expected typed error. (3) Outbox claim lifecycle — claim/complete/retry/fail transitions with status predicates: claim only-if-pending, illegal transitions rejected, terminal states final. (4) Outbox concurrent-claim contention — two concurrent claimants, exactly one wins; racing settle vs claim produces the deterministic outcome L07 established. Where the oracle framework only drives single-threaded scenarios today, extend the harness minimally to express the two contention scenarios — follow the existing scenario idiom; do not redesign the framework.",
    "context": "Source review: docs/REVIEW-23-07.md F-9/m-2/F-8. This lane STACKS ON L07 (branch from its branch): before L07, the haematite backend genuinely fails the outbox families — that is the point of the ordering. If any new scenario fails on either backend after L07, that is a real finding: report it via the coordinator rather than weakening the scenario to green. The memory reference store defines expected semantics; where memory and libSQL disagree, STOP and escalate — do not pick silently.",
    "pointers": [
      "docs/REVIEW-23-07.md (F-9, m-2, F-8)",
      "crates/aion-store/src/conformance.rs (the oracle and its scenario idiom)",
      "crates/aion-store/src/memory.rs (reference semantics; non-contiguous rejection ~303-311)",
      "crates/aion-store/src/store.rs (list_active/list_paused trait contract)",
      "crates/aion-store-libsql and crates/aion-store-haematite conformance harness call sites",
      "L07's branch (the outbox semantics being pinned)"
    ],
    "scope_in": [
      "crates/aion-store/src/conformance.rs (+ minimal harness support for contention scenarios)",
      "Both backends' conformance-suite invocation sites (wiring only)",
      "crates/aion-store/src/store.rs list_active doc comment (only if L01 has not already fixed it)"
    ],
    "scope_out": [
      "NO changes to either backend's implementation — failures found here are findings to report, not to fix in this lane",
      "NO changes to the reference memory store's semantics",
      "NO framework redesign",
      "Workspace laws: no unwrap/expect/panic (tests included), no #[allow], typed errors, files ≤500 lines"
    ],
    "acceptance": [
      "All four scenario families present in the shared oracle and running against memory, libSQL, and haematite",
      "Paused scenarios pin list_paused membership and Running-only list_active",
      "Non-contiguous scenarios pin the exact typed rejection for gapped and stale batches",
      "Outbox lifecycle scenarios pin the status-predicated transition machine; contention scenarios pin single-winner claim and deterministic racing-settle outcome",
      "Every scenario green on all three backends at this branch (stacked on L07), or any red reported as a finding with the scenario kept intact",
      "The dev report maps each new scenario to the finding it pins (F-3, F-8, F-9 traceability)"
    ],
    "notes": "The oracle is the regression fence for the whole backend family — scenario strength matters more than scenario count. A scenario that would still pass against the pre-L07 haematite outbox is too weak; check the contention scenarios against the pre-L07 code (they must fail there) and record that evidence."
  },
  "config": {
    "repo_root": "{REPO_ROOT}",
    "base_branch": "{L07_BRANCH}",
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
