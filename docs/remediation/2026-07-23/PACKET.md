# Remediation packet: 2026-07-23 repository review

**Source:** `docs/REVIEW-23-07.md` (10 domain reviews + 28 adversarial verifications; 15 major / 13 minor findings, 0 refuted). Top-3 findings independently re-confirmed at the current main bytes by Vesper before this packet was cut.

**Consumer:** the remediation coordinator (see `coordinator-brief.md`). The coordinator reads every lane brief in `lanes/`, plans the waves, dispatches one `dev_brief` run per lane, reviews and merges the resulting branches, and escalates per its brief. This manifest is data, not a plan — wave grouping and ordering are the coordinator's call, subject to the constraints below.

## Hard constraints (not the coordinator's to re-decide)

1. **Dependency edges are real.** A lane listing `depends_on` branches off its parent lane's branch and merges only after the parent lands.
2. **Risk tiers are binding.** `small` lanes flow build → review → gate → merge under the coordinator's own authority (attestation protocol). `deep_tear` lanes stop after gates and block on Vesper Lynd's recorded verdict before merging.
3. **`blocked` lanes do not dispatch** until the blocking decision is recorded in this file.
4. **Lanes within one wave must be pairwise independent** (no shared `depends_on` chain, no overlapping primary files).

## Lanes

| id | title | findings | risk | depends_on | status |
|---|---|---|---|---|---|
| L01 | Top-level documentation truth sweep | F-12 F-13 F-14 F-15 F-8 m-3 m-13 | small | — | ready |
| L02 | Gleam-dependent tests: gate-and-skip | Build&Test section | small | — | ready |
| L03 | Pause-hold poison → typed errors | F-11 m-4 m-8 | small | — | ready |
| L04 | Worker token expiry on every frame | F-2 | small | — | ready |
| L05 | `.v1` timeout-identity restamp fix | F-4 | small | — | ready |
| L06 | Positional `now()` | F-1 | deep_tear | — | ready |
| L07 | haematite outbox CAS + single-commit append | F-3 m-1 | deep_tear | — | ready |
| L08 | Conformance oracle extension | F-9 m-2 F-8(code) | small | L07 | ready |
| L09 | SDK error-taxonomy reconciliation | F-5 F-6 F-7 | small | — | ready — ruling recorded: Rust's 13 variants are canonical |
| L10 | Minors sweep | F-10 m-5 m-6 m-7 m-9 m-10 m-11 m-12 | small | — | ready |
| L11 | God-file splits (`store.rs` ~2100, `state.rs` ~1080) | standards | small | L07 | optional — dispatch last, skip if churn outweighs value |

**Suggested (non-binding) shakedown ordering:** L01+L02 alone as the first wave — zero-risk lanes that prove the pipeline before it touches the engine.

## Run configuration shared by every lane

- Repo: the aion checkout on the runner box; `base_branch` = `main` (or the parent lane's branch for stacked lanes).
- Gate battery (every lane): `cargo fmt --all` → `cargo clippy --workspace --all-targets -- -D warnings` → `cargo test --workspace`. `verify_gates` = the same battery, re-run on accept as recorded evidence.
- Workspace laws apply verbatim in every brief's `scope_out`: no `unwrap`/`expect`/`panic` (tests included; tests use `type TestResult = Result<(), Box<dyn std::error::Error>>` + `?`), no `#[allow]`/`#[expect]`/`#[ignore]`, files ≤ 500 code lines, `mod.rs` re-exports only, backticked identifiers in doc comments, typed errors.
- Commit trailer: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`. Explicit `git add` paths only — never `-A`.
- No timeouts anywhere. Norn sessions resume-if-exists; a retry resumes the session, never restarts it.

## Decisions recorded

- *(2026-07-24)* Packet cut from REVIEW-23-07.md. L09 blocked pending Tom's canonical-taxonomy ruling (Vesper's lean: keep Rust's 13; `not_owner`-terminal in three SDKs is the production bug).
- *(2026-07-24, Tom)* **Taxonomy ruling: Rust's 13 variants are canonical.** L09 unblocked; its brief was pre-written for exactly this option and dispatches as-is.
