# L01 — Top-level documentation truth sweep

**Findings:** F-12, F-13, F-14, F-15, F-8 (doc wording half), m-3, m-13 · **Risk:** small · **Depends on:** — · **Status:** ready

Docs-only lane: makes the load-bearing top-level documents tell the truth about the shipped system. Zero production code changes. Good shakedown lane — if the pipeline mangles this, nothing of value is at risk.

## dev_brief input

```json
{
  "brief": {
    "id": "rem-l01-doc-sweep",
    "title": "Top-level documentation truth sweep (REVIEW-23-07 F-12/F-13/F-14/F-15/F-8/m-3/m-13)",
    "objective": "The repository's top-level documentation has drifted badly behind the shipped code, and the drift is load-bearing: it misleads operators about where the system-of-record lives and misleads every agent onboarding from CLAUDE.md. Bring the named documents back to the truth of the code, verifying every claim you write against the current source — never against another document. Findings, each independently confirmed against source by an adversarial verifier: (1) F-12 — CLAUDE.md:17, README.md:108, and docs/design/DESIGN-OVERVIEW.md:629 all state libSQL is the default store; the shipped default is haematite (crates/aion-cli/Cargo.toml default features). State haematite as the default everywhere, with libSQL as the alternative backend. (2) F-13 — README.md:45 'Honest limits' claims 'There is no clustering'; aion-server/src/cluster.rs (~1,100 lines, multi-node failover) ships default-on. Rewrite the limits section so it is honest in BOTH directions: clustering exists and is default-on; state its actual maturity/limits by reading the cluster module and its tests, not by guessing. (3) F-14 — CLAUDE.md's architecture section omits ~9 crates (aion-awl, aion-lsp, aion-package, aion-store-haematite, aion-toolchain, aion-darwin-acl, aion-proto-generated, integration crates), claims 'twelve clusters' where docs/design has 30, and lists Python/TS SDKs as crates when they live under sdks/. Regenerate the crate list and cluster count from the actual workspace members and docs/design contents. (4) F-15 — AWL (aion awl check/fmt/emit/schema, aion run <file.awl>, direct .awl deploy) is a shipped first-class authoring surface with zero mentions in README/GETTING-STARTED/CLAUDE.md. Add it to the authoring story in all three, proportionate to the existing Gleam coverage. (5) F-8 doc half — aion-store/src/store.rs:101 list_active doc says 'non-terminal' while both shipped backends filter Running-only and the list_paused doc says so; fix the list_active doc comment to state the Running-only contract explicitly (doc comment ONLY — no behavior change). (6) m-3 — DESIGN.md/CLAUDE.md describe 5 WorkflowStatus variants; aion-core/src/status.rs has 7 (ContinuedAsNew, Paused). Enumerate all 7 wherever variants are listed. (7) m-13 — docs/design/workflow-engine/COMPONENT-ARCHITECTURE.md names store crates that don't exist (postgres/sqlite/turso) and DESIGN-OVERVIEW code samples import aion_store_postgres; correct to the real crates (aion-store-libsql, aion-store-haematite) and update the samples to compile against real crate names.",
    "context": "Source review: docs/REVIEW-23-07.md (read it first — the findings above are its F-numbers, each carries a verified file:line). The convergence to beamr 0.16 / haematite 0.7 / liminal 0.4 landed 2026-07-23; version references you encounter should match the 0.10.0 family (aion 0.10.0, beamr 0.16, haematite 0.7, liminal 0.4) — but do not do a version sweep beyond the named findings.",
    "pointers": [
      "docs/REVIEW-23-07.md",
      "CLAUDE.md",
      "README.md",
      "GETTING-STARTED.md",
      "docs/design/DESIGN-OVERVIEW.md",
      "docs/design/workflow-engine/COMPONENT-ARCHITECTURE.md",
      "crates/aion-cli/Cargo.toml",
      "crates/aion-server/src/cluster.rs",
      "crates/aion-core/src/status.rs",
      "crates/aion-store/src/store.rs"
    ],
    "scope_in": [
      "CLAUDE.md",
      "README.md",
      "GETTING-STARTED.md",
      "docs/design/DESIGN-OVERVIEW.md",
      "docs/design/workflow-engine/COMPONENT-ARCHITECTURE.md",
      "DESIGN.md (m-3 variant list only, if present)",
      "crates/aion-store/src/store.rs (list_active doc comment ONLY)"
    ],
    "scope_out": [
      "NO production code changes anywhere — the single permitted source edit is the list_active doc comment",
      "NO changes to docs/design JSON sources or the design-system tooling",
      "NO restructuring of any document — correct claims in place; do not reorganize sections",
      "NO version sweeps beyond the named findings",
      "Workspace laws apply to any doc-comment text: backticked identifiers"
    ],
    "acceptance": [
      "Every document in scope states haematite as the default store and libSQL as the alternative; zero remaining claims of a libSQL default (grep evidence required)",
      "README limits section accurately describes shipped clustering with claims traceable to cluster.rs or its tests",
      "CLAUDE.md crate list matches the workspace members exactly (every crates/ member present, nothing listed that is not a workspace member; SDKs identified as sdks/, not crates); cluster count matches docs/design",
      "AWL appears in the authoring story of README, GETTING-STARTED, and CLAUDE.md with the real CLI verbs",
      "list_active doc comment states the Running-only contract and no longer says 'non-terminal'; no behavior change (diff shows doc lines only)",
      "All WorkflowStatus enumerations list all 7 variants",
      "COMPONENT-ARCHITECTURE.md and DESIGN-OVERVIEW samples reference only crates that exist in the workspace",
      "Every factual claim added was verified against current source, and the dev report says where each was checked"
    ],
    "notes": "Truth first time: verify each claim at the bytes before writing it. Where the review's line numbers have drifted (it baselined 8 commits before current main), find the current location rather than trusting the citation."
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
