# L05 — `.v1` timeout-identity restamp fix

**Findings:** F-4 (major, durability) · **Risk:** small · **Depends on:** — · **Status:** ready

Closes the v1 instance of the regression class already fixed for v3: persisted `.v1` packages are silently dropped on restart because persisting re-stamps them with a hash that no longer matches their store key.

## dev_brief input

```json
{
  "brief": {
    "id": "rem-l05-v1-restamp",
    "title": "Persisted .v1 packages must survive restart: fix the to_archive_bytes restamp (F-4)",
    "objective": "crates/aion-package promises .v1 single-value timeout identities recover on restart: verified_content_hash deliberately accepts them (see the comment in hash.rs around lines 172-176, 'so a .v1-stamped deployment recovers on restart instead of being skipped'). But to_archive_bytes (package.rs, around line 231) re-stamps v1 manifests with the beams-only legacy hash — the has_explicit_timeout_identity predicate is false for v1 — so the persisted record's store key no longer matches the recomputed hash on reload, reload_persisted_packages skips the package, and routes fail with UnknownVersion. This is a durability break for a form the loader explicitly promises to keep recoverable, and it is the exact regression class already fixed for .v3 (see the v3 round-trip regression test in deploy_persistence_e2e.rs around line 420). Fix it the same way the v3 case was fixed: a persisted v1 package's archive bytes must round-trip to the same verified content hash it was stored under. Preserve the v1 identity through to_archive_bytes rather than widening what verified_content_hash accepts — the hash-acceptance side is already correct; the persist side is what lies. Add the v1 round-trip regression test in the same suite and shape as the v3 one: deploy v1-stamped package, persist, reload, assert it is recovered and routable, not skipped.",
    "context": "Source review: docs/REVIEW-23-07.md F-4, adversarially verified. The review baselined 8 commits behind current main — re-locate drifted line numbers. Study the v3 fix FIRST and mirror it; if the v3 approach genuinely cannot apply to v1, stop and report why in the dev report rather than inventing a divergent mechanism. Do not touch the content-hash algorithm itself: catalog immutability (ManifestMismatch tripwire) and hash compatibility for already-persisted stores are hard constraints — an already-persisted v1 record written by the CURRENT buggy code must also be considered: state in the dev report whether such records exist in the wild (persisted under the legacy hash) and whether they reload after the fix; if they cannot, say so explicitly — do not silently strand them.",
    "pointers": [
      "docs/REVIEW-23-07.md (F-4)",
      "crates/aion-package/src/package.rs (to_archive_bytes, has_explicit_timeout_identity)",
      "crates/aion-package/src/hash.rs (verified_content_hash, the v1-acceptance comment)",
      "deploy_persistence_e2e.rs (the v3 round-trip regression test — the mold for this fix)",
      "reload_persisted_packages and the UnknownVersion route-failure path"
    ],
    "scope_in": [
      "crates/aion-package (the restamp path and its tests)",
      "deploy persistence e2e suite (the new v1 round-trip test)"
    ],
    "scope_out": [
      "NO changes to the content-hash algorithm or length-framed hashing",
      "NO changes to verified_content_hash's acceptance set",
      "NO changes to .v2/.v3 handling beyond shared code the mirror requires",
      "NO catalog or manifest format changes",
      "Workspace laws: no unwrap/expect/panic (tests included), no #[allow], typed errors, files ≤500 lines"
    ],
    "acceptance": [
      "A .v1-stamped package deployed, persisted, and reloaded is recovered and routable (round-trip regression test, same shape as the v3 one, passing)",
      "to_archive_bytes output for a v1 package hashes to the key it was stored under (unit-level assertion)",
      "The v3 round-trip test and all existing package tests remain green",
      "The dev report states the fate of records persisted by the pre-fix code and why"
    ],
    "notes": "Small, sharp lane. The pre-fix-persisted-records question is a report obligation, not necessarily a code obligation — surfacing the truth is the acceptance bar."
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
