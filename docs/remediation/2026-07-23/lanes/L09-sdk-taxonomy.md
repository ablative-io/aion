# L09 — SDK error-taxonomy reconciliation

**Findings:** F-5 (major — production bug in three SDKs), F-6, F-7 · **Risk:** small · **Depends on:** — · **Status:** **BLOCKED: Tom's canonical-taxonomy ruling (10 vs 13)**

Do not dispatch until the ruling is recorded in `PACKET.md`. The brief below is written for **Option 13** (Vesper's recommendation: adopt Rust's 13-variant taxonomy as canonical); if Tom rules for 10, the objective's direction inverts (collapse Rust to the contract) and the brief must be re-cut before dispatch.

## dev_brief input (Option 13 — pending ruling)

```json
{
  "brief": {
    "id": "rem-l09-sdk-taxonomy",
    "title": "Reconcile the client error taxonomy across all four SDKs + CLIENT-CONTRACT.md (F-5/F-6/F-7)",
    "objective": "The four client SDKs have diverged three ways on the error taxonomy, and the divergence is a production bug: wire code 13 (not_owner — a routine wrong-shard-owner fence in HA deployments) is retryable Unavailable in Rust (crates/aion-client/src/error.rs, ~line 319: re-resolve + retry) but Python/TS/Gleam never decode codes 11-14 and collapse it to a terminal Server error — so a fence Rust callers ride through hard-fails every other SDK. Code 14 (invalid_state, caller-reachable reopen-precondition failure) is classified three different ways. RULING (recorded in PACKET.md): the canonical taxonomy is Rust's 13 variants. Work: (1) Update CLIENT-CONTRACT.md to specify the 13-variant taxonomy, including the retryability class of every wire code 1-14 — the contract document leads, code follows it. (2) Bring Python (sdks/python), TypeScript (sdks/typescript), and Gleam SDKs to the contract: decode codes 11-14 into the same variants with the same retryability semantics as Rust — not_owner triggers the same re-resolve-and-retry behavior in every SDK's retry machinery. (3) Verify Rust against the updated contract and fix any residual divergence. (4) F-7 — add OFFLINE wire-code mapping fixtures to the client conformance suite (conformance/aion-clients/): a fixture table of every wire code → expected variant + retryability per SDK, runnable WITHOUT a live server (the current 7 scenarios assert only 4 taxonomy members and skip entirely without AION_SERVER_URL, so CI verifies none of the mapping). Every SDK's fixture check runs in ordinary CI.",
    "context": "Source review: docs/REVIEW-23-07.md F-5/F-6/F-7. The three-SDK parity engineering on reconnect/backoff/unacked-replay was verified line-for-line identical — mirror that discipline: same mapping table, same retry classification, verified at matching sites in all four SDKs. This is a wire-COMPATIBLE change (no server changes, no code renumbering): all four SDKs currently receive codes 11-14; three of them just fail to decode them.",
    "pointers": [
      "docs/REVIEW-23-07.md (F-5, F-6, F-7)",
      "CLIENT-CONTRACT.md",
      "crates/aion-client/src/error.rs (~296-330: the 13-variant mapping — the reference)",
      "sdks/python and sdks/typescript error mapping + retry machinery",
      "the Gleam SDK error mapping",
      "conformance/aion-clients/scenarios.json and its harness"
    ],
    "scope_in": [
      "CLIENT-CONTRACT.md",
      "Error mapping + retry classification in all four SDKs",
      "conformance/aion-clients (offline fixtures + harness support to run them without a server)",
      "SDK unit tests for the mappings"
    ],
    "scope_out": [
      "NO server-side changes; NO proto/wire changes; NO code renumbering",
      "NO changes to reconnect/backoff machinery beyond routing the newly-decoded retryable variants into it",
      "NO live-server conformance changes (the live suite stays as-is; fixtures are additive)",
      "Workspace laws for Rust; each SDK's established idioms for Python/TS/Gleam"
    ],
    "acceptance": [
      "CLIENT-CONTRACT.md specifies all 13 variants with per-wire-code retryability; the dev report shows the 4-SDK mapping table side by side",
      "not_owner is retryable with re-resolve semantics in all four SDKs (per-SDK test evidence)",
      "invalid_state maps to the same variant in all four SDKs",
      "Offline fixtures cover every wire code 1-14 for every SDK and run without AION_SERVER_URL in ordinary CI (evidence: fixture run output, no server)",
      "A deliberately-wrong mapping fails the fixture check (demonstrated once, then reverted — the fixture must be shown capable of catching drift)",
      "All existing SDK suites green"
    ],
    "notes": "Four SDKs in one lane is deliberate — the whole point is one taxonomy, and splitting per-SDK would recreate the drift this lane exists to end. The Rust mapping is the reference implementation; port its table, not an interpretation of it."
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

**Coordinator note:** the SDK test batteries (Python/TS/Gleam) are outside the Rust gate battery — the brief's acceptance criteria require per-SDK test evidence, and the review lenses must hold the diff to it. If the runner box lacks a working Python/Node/Gleam toolchain for the SDK suites, surface that at dispatch time rather than letting the lane claim untested parity.
