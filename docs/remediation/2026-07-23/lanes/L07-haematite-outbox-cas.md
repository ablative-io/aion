# L07 — haematite outbox CAS + single-commit append

**Findings:** F-3 (major, concurrency — on the DEFAULT backend), m-1 · **Risk:** **deep_tear** · **Depends on:** — · **Status:** ready

Brings the haematite backend's outbox path up to the contract libSQL implements: guarded state transitions and a single durability point for append+outbox. **This lane blocks on Vesper Lynd's recorded APPROVE before merge.**

## dev_brief input

```json
{
  "brief": {
    "id": "rem-l07-haematite-outbox-cas",
    "title": "Guard haematite outbox transitions with CAS; unify append_with_outbox durability (F-3/m-1)",
    "objective": "haematite is the shipped DEFAULT backend, and its outbox path diverges from the store contract in two verified ways. (1) F-3 — crates/aion-store-haematite/src/store.rs: claim/transition/settle (around lines 1880, 2446, 2500) are unguarded read-modify-write — scan-or-get, unconditional put_routed, commit — with no CAS, no status predicate, and no serialization (self.blocking is a bare spawn_blocking, ~1060-1069). libSQL, the oracle it must match, does select-and-claim in one IMMEDIATE transaction with WHERE status='pending' guards on every update. Consequence: two same-node racers (dispatcher claim vs cancel-settle; reconciler vs worker complete) each read old state and last-write-wins — a cancelled ordinal can dispatch anyway, and terminal outbox status is non-deterministic. haematite exposes a cas primitive; use it. Every outbox state transition must carry the equivalent of libSQL's status predicate: claim only-if-pending, settle/complete only from the states libSQL permits — enumerate libSQL's transition guards first and mirror them exactly, so the two backends implement one state machine. (2) m-1 — append_with_outbox (~line 1370) commits events and outbox rows at two separate durability points; libSQL does one transaction, and the contract says both-or-neither is load-bearing. Unify to a single durability point (one commit covering both trees) if haematite's transaction surface allows it; if it genuinely does not, implement the closest achievable ordering, prove the crash-window behavior with a test, and report the residual gap explicitly — do not paper over it. Required tests: same-node race tests for dispatcher-claim vs cancel-settle and reconciler vs worker-complete (loser of the race must observe a CAS failure and re-read, never blind-overwrite — assert the terminal status is deterministic); a claim-contention test (two concurrent claimants, exactly one wins); an append_with_outbox atomicity/crash-window test.",
    "context": "Source review: docs/REVIEW-23-07.md F-3/m-1, adversarially verified. Bounding (verified, and to be preserved): history-level exactly-once holds via Recorder dedup regardless of outbox races, and cross-node double-claim is prevented by shard ownership — this lane fixes SAME-NODE determinism, and must not weaken either existing property. The libSQL implementation is the reference semantics — read crates/aion-store-libsql's outbox transaction shapes before designing. haematite 0.7.0 is the pinned version; work within its published API (cas primitive) — if a haematite-side capability gap blocks the single-commit unification, that is a REPORT outcome (escalate via the coordinator; haematite changes are out of scope for this lane and would route to the haematite repo separately).",
    "pointers": [
      "docs/REVIEW-23-07.md (F-3, m-1)",
      "crates/aion-store-haematite/src/store.rs (claim/transition/settle sites, self.blocking, append_with_outbox)",
      "crates/aion-store-libsql/src/ (the outbox transaction shapes — the reference semantics)",
      "haematite 0.7.0 cas primitive (registry docs / haematite crate source)",
      "crates/aion-store/src/ (the outbox contract surface both backends implement)"
    ],
    "scope_in": [
      "crates/aion-store-haematite/src/store.rs (outbox paths, append_with_outbox, and the minimal supporting structure)",
      "crates/aion-store-haematite tests"
    ],
    "scope_out": [
      "NO changes to the shared store contract/trait surfaces (that is L08's territory, stacked after this lane)",
      "NO changes to the libSQL backend",
      "NO changes to the haematite crate itself (capability gaps are report-and-escalate)",
      "NO changes to Recorder dedup or shard-ownership mechanisms",
      "NO weakening of the existing collect-replay re-derivation mitigation",
      "Workspace laws: no unwrap/expect/panic (tests included), no #[allow], typed errors; note store.rs is already over the 500-line law — do NOT reorganize it in this lane (that is L11); keep the net line growth minimal"
    ],
    "acceptance": [
      "Every outbox state transition in the haematite backend is CAS-guarded with status predicates mirroring libSQL's transition guards (side-by-side enumeration of both backends' guards in the dev report)",
      "Race tests: dispatcher-claim vs cancel-settle and reconciler vs worker-complete produce deterministic terminal status; the losing racer observes CAS failure and re-reads (tests demonstrated to fail against the pre-fix code, evidence in the dev report)",
      "Claim contention: exactly one of two concurrent claimants wins (test)",
      "append_with_outbox commits events and outbox rows at one durability point, with a crash-window test — or the residual gap is explicitly proven, tested, and reported",
      "Existing haematite conformance and backend suites green; no regression in the collect-replay path"
    ],
    "notes": "DEEP TEAR LANE: after gates pass, the coordinator does NOT merge — it escalates to Vesper Lynd with branch, dev report, race-test red-then-green evidence, the guard-enumeration table, and every lens verdict, and blocks on her recorded verdict. L08 (conformance-oracle extension) stacks on this branch: it pins these behaviors into the shared oracle so neither backend can drift again."
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
