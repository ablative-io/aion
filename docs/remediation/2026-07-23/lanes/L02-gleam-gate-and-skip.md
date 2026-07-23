# L02 — Gleam-dependent tests: gate-and-skip

**Findings:** REVIEW-23-07 Build & Test Health section · **Risk:** small · **Depends on:** — · **Status:** ready

Makes `cargo test --workspace` green on a machine without the `gleam` CLI, per the repo's own runtime-gating rule. Test-code-only lane.

## dev_brief input

```json
{
  "brief": {
    "id": "rem-l02-gleam-gate",
    "title": "Gleam-dependent tests gate-and-skip on missing toolchain (REVIEW-23-07 Build & Test Health)",
    "objective": "130 tests across 45 suites (compile-proof, archive-gate, codec-proof families) hard-error when the gleam CLI is not on PATH, instead of gating. CLAUDE.md's rule for runtime-dependent tests: detect the missing runtime, log a skip, and return Ok(()) — never fail on a missing toolchain. Bring every gleam-dependent test in the workspace to that rule. The codebase already has gleam_available()-style helpers in several test files — reuse/extend that pattern rather than inventing a new one; hoist a shared helper into a common test-support location if one exists, otherwise keep the established per-crate pattern consistent. Additionally, .github/workflows/ci.yml (the gleam install step around lines 148-153) carries a comment claiming these tests would be 'skipped' without gleam — after this change that comment becomes true; reword it to describe the actual gate-and-skip behavior. CI must continue to install gleam so the full suite still RUNS these tests there — this lane changes local-developer behavior only, never CI coverage.",
    "context": "Source review: docs/REVIEW-23-07.md, Build & Test Health. All 130 failures were verified to be this single environmental cause; no logic failures. The two engine::builder NotFound tests seen failing on gleam-less hosts during the 2026-07-23 convergence are part of this same class. Find the full set empirically: run cargo test --workspace with gleam removed from PATH and enumerate every failure — do not trust a static grep to find all 45 suites.",
    "pointers": [
      "docs/REVIEW-23-07.md",
      "CLAUDE.md (runtime-gating rule for runtime-dependent tests)",
      ".github/workflows/ci.yml",
      "existing gleam_available() helpers (grep the workspace tests for gleam_available / which gleam patterns)"
    ],
    "scope_in": [
      "Test files and test-support modules across the workspace",
      ".github/workflows/ci.yml (comment wording only)"
    ],
    "scope_out": [
      "NO production (non-test) code changes",
      "NO removal or permanent disabling of any test — a gated test must still run fully when gleam IS on PATH",
      "NO #[ignore] attributes — the gate is a runtime check with a logged skip and Ok(()), per the workspace law banning #[ignore]",
      "NO changes to CI's gleam installation — CI keeps running these tests",
      "Workspace laws apply: no unwrap/expect/panic in tests; tests return TestResult with ?"
    ],
    "acceptance": [
      "cargo test --workspace exits 0 on a machine WITHOUT gleam on PATH, with every gated test logging an explicit skip (evidence: full run output captured with gleam removed from PATH)",
      "cargo test --workspace on a machine WITH gleam still executes the gated tests for real — the skip path is provably not taken (evidence: run output showing the suites executing)",
      "Every one of the 130 empirically-enumerated failures is covered — the dev report lists the enumerated set and its count",
      "ci.yml comment describes the actual behavior",
      "The gating helper pattern is consistent across all touched suites (one idiom, not 45 variants)"
    ],
    "notes": "Enumerate first, sweep second. The dev report must include the before-state failure count reproduced locally so the after-state claim (0 failures, N skips) is checkable against it."
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
