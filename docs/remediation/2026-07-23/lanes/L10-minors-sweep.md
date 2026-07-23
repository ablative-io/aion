# L10 — Minors sweep

**Findings:** F-10, m-5, m-6, m-7, m-9, m-10, m-11, m-12 · **Risk:** small · **Depends on:** — · **Status:** ready

Eight small, independent corrections across six crates. One lane because each is minutes-to-hours of work; the review lenses hold each item separately.

## dev_brief input

```json
{
  "brief": {
    "id": "rem-l10-minors-sweep",
    "title": "Minors sweep: 8 verified small findings (F-10, m-5/6/7/9/10/11/12)",
    "objective": "Eight independent items, each adversarially verified in docs/REVIEW-23-07.md; fix each in place, smallest-correct-change style. (1) F-10 — crates/aion/src/runtime/nif_timer_bridge.rs (~line 514): deadline-fire retry policy hardcoded as consts (MAX_ATTEMPTS=6 / 200ms / 30s) where CLAUDE.md names retry policy builder-supplied; the co-located child-terminal watcher already threads a builder-supplied SignalDeliveryConfig — thread a builder-supplied config for deadline-fire the same way, with the current values as the builder's defaults (behavior unchanged for existing callers). (2) m-5 — crates/aion/src/signal/resume.rs (~171): delete the unused SignalResumeError::Deliver variant and any dead handling of it. (3) m-6 — crates/aion-package/src/structure/determinism.rs (~298): the determinism linter silently misses aliased imports (import gleam/float as f) and cross-module helpers while claiming 'no silent miss'. Harden alias resolution so aliased module imports ARE caught; for cross-module helpers, if detection is not achievable in this linter's design, narrow the documented claim (doc comment + any user-facing text for aion check-deterministic) to state exactly what is and is not detected — the claim and the behavior must match, whichever moves. Add lint tests for the aliased-import case. (4) m-7 — crates/aion-client/src/payload.rs (~40): Rust from_payload skips the content-type tag check Python/TS enforce; add the same check with the same error semantics, with a test. (5) m-9 — crates/aion-awl/src/mir/lower/codec_encode.rs (~104): index casts saturate to u16::MAX/u32::MAX, a silent miscompile at absurd sizes; the functions already return Result — replace saturation with a typed overflow error, tested at the boundary. (6) m-10 — crates/aion-server/src/worker/quota_cache.rs (~91): a store Err is folded into 'no override' with zero logging, silently admitting every tenant at platform_default under a persistent registry fault; keep the fail-open availability behavior but log the error at error level with enough context to alert on (and a test asserting the log-on-error path). (7) m-11 — crates/aion/src/runtime/nif_activity_retry.rs (~147): remove the one production #[allow(clippy::cast_*)] by reworking the site to checked/typed casts that satisfy clippy without the allow. (8) m-12 — crates/aion-server/src/run.rs (~36): PLACEMENT_CACHE_TTL / QUOTA_CACHE_TTL / QUOTA_BROADCAST_CADENCE are hardcoded consts while sibling cadences are plumbed from config; plumb them through the same config path with the current values as defaults.",
    "context": "Every item is independent — implement and commit them as separate commits within the lane branch (one commit per item, item id in the commit subject) so review and any selective revert stay surgical. Line numbers baselined 8 commits behind current main; re-locate at the current tree. For F-10 and m-12, 'defaults preserve current behavior' is a hard requirement — these are configurability changes, not tuning changes.",
    "pointers": [
      "docs/REVIEW-23-07.md (F-10, m-5, m-6, m-7, m-9, m-10, m-11, m-12)",
      "crates/aion/src/runtime/nif_timer_bridge.rs + the child-terminal watcher's SignalDeliveryConfig threading (the mold for F-10)",
      "crates/aion/src/signal/resume.rs",
      "crates/aion-package/src/structure/determinism.rs",
      "crates/aion-client/src/payload.rs + the Python/TS from_payload equivalents (the check being mirrored)",
      "crates/aion-awl/src/mir/lower/codec_encode.rs",
      "crates/aion-server/src/worker/quota_cache.rs",
      "crates/aion/src/runtime/nif_activity_retry.rs",
      "crates/aion-server/src/run.rs + the config plumbing of its sibling cadences"
    ],
    "scope_in": [
      "Exactly the files/sites named per item, plus minimal config/builder surface for F-10 and m-12, plus tests per item"
    ],
    "scope_out": [
      "NO behavior changes beyond each item's stated fix (defaults preserve current values for F-10/m-12; m-10 stays fail-open)",
      "NO drive-by refactors of touched files",
      "NO new #[allow] anywhere (m-11 removes one; adding any is an automatic reject)",
      "Workspace laws: no unwrap/expect/panic (tests included), typed errors, files ≤500 lines"
    ],
    "acceptance": [
      "One commit per item, item id in the subject; the dev report maps each item to its commit and its test",
      "F-10/m-12: builder/config-supplied with current values as defaults; a test proves the default path is byte-equal behavior",
      "m-5: variant gone, workspace compiles, zero references remain",
      "m-6: aliased-import lint test passes; the documented claim matches actual detection exactly",
      "m-7: Rust rejects a wrong content-type tag with the same semantics as Python/TS (test)",
      "m-9: boundary overflow yields a typed error, not a saturated index (test at the boundary)",
      "m-10: store error logged at error level with actionable context; fail-open behavior preserved (both tested)",
      "m-11: the #[allow] is gone and clippy is clean at -D warnings",
      "Full battery green"
    ],
    "notes": "Eight small items is where scope creep breeds — the brief_compliance lens should hold the diff hard to the named sites."
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
