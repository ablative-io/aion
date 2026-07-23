# L11 — God-file splits

**Findings:** standards audit (no-god-files rule: >500 production LOC) · **Risk:** small (mechanical, but churny) · **Depends on:** **L07** · **Status:** optional — dispatch last; skip if churn outweighs value at the time

Splits the two files far over the 500-line law. Deliberately last: `store.rs` is L07's battlefield and `state.rs` is broadly load-bearing — this lane must not collide with functional lanes.

## dev_brief input

```json
{
  "brief": {
    "id": "rem-l11-god-file-splits",
    "title": "Split aion-store-haematite/src/store.rs (~2,100 LOC) and aion-server/src/state.rs (~1,080 LOC) per the no-god-files law",
    "objective": "Two files are far over the workspace's 500-production-LOC law: crates/aion-store-haematite/src/store.rs (~2,100) and crates/aion-server/src/state.rs (~1,080). Split each into cohesive submodules by existing responsibility seams — store.rs plausibly along append/read/outbox/visibility/keyspace lines, state.rs along its existing responsibility clusters; derive the actual split from the code's own structure, not from this guess. MECHANICAL MOVES ONLY: no behavior changes, no signature changes, no visibility widening beyond what module boundaries force (and where forced, keep it pub(crate) and note each in the dev report). mod.rs files contain re-exports only, per the workspace law. Public API of both crates byte-identical before and after (verify: cargo public-api or an equivalent diff of the crates' public surfaces, included as evidence). Near-the-line files (worker/bridge.rs ~800, worker/registry.rs ~600) are OUT of scope — two files, done well.",
    "context": "Standards finding from REVIEW-23-07.md's first-hand audit. This lane deliberately runs LAST and stacks on L07 (store.rs will have changed under it). If other packet lanes are still unmerged when this dispatches, the coordinator must re-check independence at dispatch time. Mechanical-move discipline: each commit moves one cohesive cluster; the diff for each commit should be recognizable as a move (deletions in one file, matching additions in a new module) — the reviewers will hold the diff to move-only.",
    "pointers": [
      "CLAUDE.md (no-god-files law, mod.rs re-exports-only law)",
      "crates/aion-store-haematite/src/store.rs (post-L07 state)",
      "crates/aion-server/src/state.rs",
      "existing multi-module crates in the workspace (the established module-layout idiom)"
    ],
    "scope_in": [
      "crates/aion-store-haematite/src/ (store.rs split + new submodules + mod wiring)",
      "crates/aion-server/src/ (state.rs split + new submodules + mod wiring)",
      "Test module relocations that the moves force"
    ],
    "scope_out": [
      "NO behavior changes, NO signature changes, NO renames of public items",
      "NO splitting of any file other than the two named",
      "NO test changes beyond relocation/imports",
      "Workspace laws: files ≤500 code lines INCLUDING the new modules — a split that leaves a 700-line module is not done"
    ],
    "acceptance": [
      "Both named files and every new module ≤500 production code lines (line-count evidence in the dev report)",
      "Public API surfaces of both crates identical before/after (tool-diff evidence)",
      "Every commit is demonstrably move-only; forced visibility changes enumerated with justification",
      "mod.rs files are re-exports only",
      "Full battery green with zero test-logic changes"
    ],
    "notes": "If, at dispatch time, the coordinator judges the merge-conflict risk against in-flight work too high, skipping this lane entirely is the sanctioned outcome — record the skip and the reason in PACKET.md rather than forcing it through."
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
