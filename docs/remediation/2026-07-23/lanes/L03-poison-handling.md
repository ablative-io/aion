# L03 — Lock-poison handling → typed errors

**Findings:** F-11 (major), m-4, m-8 · **Risk:** small · **Depends on:** — · **Status:** ready

Closes the pause-hold poison hole and unifies the workspace's three remaining silent-poison sites onto the typed-error discipline the rest of the code already follows.

## dev_brief input

```json
{
  "brief": {
    "id": "rem-l03-poison-handling",
    "title": "Map lock poison to typed errors: PausedRuns, InMemoryStore::lock_namespaces, mock agent harness (F-11/m-4/m-8)",
    "objective": "Three sites silently swallow lock poison; the most serious defeats a durability guarantee. (1) F-11 — crates/aion/src/lifecycle/pause.rs: PausedRuns::insert/remove/replace_all/extend use `if let Ok` and drop the poison arm with no log; snapshot() returns an EMPTY held set on poison via unwrap_or_default(). Consequence: once poisoned, the outbox dispatcher's pause-hold exclusion (crates/aion/src/lifecycle/outbox_dispatcher.rs, the snapshot-based exclusion around lines 452-453) silently stops excluding paused runs, and a durably-Paused workflow's activities dispatch anyway until restart. Rework PausedRuns so every method surfaces poison as the crate's existing typed error family, and rework every caller — the dispatcher especially — to fail loudly (refuse the claim round with a logged typed error) rather than proceed with an empty or stale set. An empty-on-poison snapshot must be impossible by construction. (2) m-4 — crates/aion-store/src/memory.rs (lock_namespaces, around line 103) recovers poison in place while sibling lock_state maps it to a typed error; align lock_namespaces with lock_state. (3) m-8 — crates/aion-integration-cli/src/mock.rs (around line 139) swallows Mutex poison with `if let Ok`, inconsistent with its own sibling intervene path; align it. Follow each crate's existing poison-to-typed-error idiom — this is a consistency lane, not a redesign; do not introduce a new error type where a fitting variant exists.",
    "context": "Source review: docs/REVIEW-23-07.md F-11/m-4/m-8, all independently verified; F-11 additionally re-confirmed at current main bytes by Vesper (four if-let-Ok writes plus snapshot().unwrap_or_default()). The review baselined 8 commits behind current main — re-locate drifted line numbers at the current tree. The rest of the workspace maps poison to typed errors; these are the deviations.",
    "pointers": [
      "docs/REVIEW-23-07.md (F-11, m-4, m-8)",
      "crates/aion/src/lifecycle/pause.rs",
      "crates/aion/src/lifecycle/outbox_dispatcher.rs",
      "crates/aion-store/src/memory.rs (compare lock_namespaces with lock_state)",
      "crates/aion-integration-cli/src/mock.rs (compare with its intervene path)",
      "crates/aion-server/src/api/worker_grpc.rs heartbeat arm (reference for the loud-on-poison logging idiom)"
    ],
    "scope_in": [
      "crates/aion/src/lifecycle/pause.rs",
      "crates/aion/src/lifecycle/outbox_dispatcher.rs (poison-path handling at PausedRuns call sites only)",
      "Other PausedRuns call sites, minimally, as the new signatures require",
      "crates/aion-store/src/memory.rs (lock_namespaces poison path only)",
      "crates/aion-integration-cli/src/mock.rs (poison path only)",
      "Tests for all of the above"
    ],
    "scope_out": [
      "NO changes to pause/resume semantics, the durable Paused projection, or list_paused",
      "NO changes to outbox claim logic beyond the poison-path refusal",
      "NO new error types where an existing variant fits",
      "NO reorganization of any touched file",
      "Workspace laws: no unwrap/expect/panic (tests included), no #[allow], typed errors, files ≤500 lines"
    ],
    "acceptance": [
      "Every PausedRuns method surfaces poison as a typed error; no code path can observe an empty set because of poison (evidence: test that poisons the lock and asserts the typed error, plus a dispatcher-level test asserting a paused run is NOT dispatched when the pause-hold read fails)",
      "The dispatcher logs and refuses the affected claim round on poison rather than dispatching",
      "lock_namespaces maps poison identically to lock_state (test included)",
      "mock.rs poison path matches its intervene sibling (test included)",
      "No silent `if let Ok` over a poisoned-lock Result remains in any touched file"
    ],
    "notes": "The subtle part is the dispatcher: refusing the round must not wedge the dispatcher loop permanently — a poisoned lock stays poisoned, so decide (and document in the dev report) whether the refusal is per-round with retry or escalates to a supervised restart of the component, following whatever supervision idiom the dispatcher already has for unrecoverable states."
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
