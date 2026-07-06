# remediation — the yggdrasil remediation flow on Aion

The finding-to-fix pipeline of `yggdrasil/docs/design/remediation-flow/DESIGN.md`
(+ DECISIONS.md D1–D8) as an Aion workflow family, built on the
`examples/pipeline-run` patterns: a signed wave plan drives serial STRATA of
parallel per-brief CHILD workflows; every agent step is a driven Norn agent
constrained by an `--output-schema`; every mechanical check is a shell
activity whose non-zero exits are recorded data, never errors.

## Topology

```
remediation_wave (PARENT)                    remediation_brief (CHILD, one per brief)
  validate strata (pure)                       provision_workspace   (shell)
  for each stratum, serially:                  test_author           (agent)  <- recommendation STRIPPED at the codec layer
    spawn remediation_brief per brief,         [coverage/routing]    (pure)      corrections: >=1 test or could_not_reproduce;
      in parallel; await all                                                     every entry has an evidence channel;
  wave report skeleton (pure metrics)                                            runnable entries carry a failure signature
                                               gate1                 (shell)     FULLY MECHANICAL: tests committed; each FAILS
                                                                                 WITH its expected_failure_signature; authored
                                                                                 diff touches ONLY test paths; manual_acceptance
                                                                                 entries echoed (nothing runs)
                                               +--> developer        (agent)  <- FULL entries incl. recommendation
                                               |    gate2            (shell)     authored-test diff empty, clippy -D warnings,
                                               |    [accounting]     (pure)      suite green + every brief finding in EXACTLY
                                               |                                 ONE of findings_addressed | findings_bounced
                                               |    verifier         (agent)     per-finding rulings (verdict.schema.json)
                                               |    [derive-check]   (pure)      overall re-derived from rulings; a disagreeing
                                               |                                 assertion (or missing reject_reason) is itself
                                               |                                 a rejection, recorded on the result
                                               +--< loop while the DERIVED overall is not accept (or the verdict is
                                                    inconsistent), cycle-capped (config.max_fix_cycles, default 3);
                                                    exhaustion = TERMINAL DISPOSITION, never a silent success
                                               ledger_update x N     (shell)     one applier call per artifact
                                               cleanup_workspace     (shell)     dirty worktrees are LEFT IN PLACE
```

- The brief's identity is materialized per child: each brief gets its own
  child workflow id, which keys the worktree (`/tmp/aion-remediation/ws/<id>`)
  and the Norn sessions (`<id>-test-author`, `<id>-developer`,
  `<id>-verifier`, resumed across the brief's fix cycles); the brief id names
  the branch (`remediation/<brief-id>`).
- `could_not_reproduce` manifest entries are carried through to the brief
  result for the operator — no automated reroute (DECISIONS.md D4).
- A `re_auditor` role connection (Stage 4) is served by the worker for the
  wave-level re-audit to come; no workflow here dispatches it yet.

## Schemas

`schemas/` contains copies of the six contract schemas from
`yggdrasil/docs/design/remediation-flow/schemas/`
(`brief`, `ledger-entry`, `test-manifest`, `fix-report`, `verdict`,
`wave-report`). **yggdrasil's copies are the source of truth** until the
schemas ship in a crate; re-sync on change. Two additions are authored here:

- `re-audit-findings.schema.json` — the re-auditor's output (yggdrasil ships
  no findings schema yet; mirrors the re-auditor profile's output contract).
- `brief_input/brief_output/wave_input/wave_output.json` — the workflow I/O
  schemas `aion package` consumes (this package's own wire, not a yggdrasil
  contract).

The agent-facing `--output-schema` documents (`test-manifest`, `fix-report`,
`verdict`, `re-audit-findings`) are embedded into the worker with
`include_str!` and drift-guarded by `worker/tests/schemas_valid.rs` against
the Gleam codecs' field sets.

The 2026-07-07 contract pass (yggdrasil 93ef89d8 + dfd05c14) folded the
manifest's `test_file`/`expected_failure_signature`/`could_not_reproduce_reason`/
`manual_acceptance`, the fix report's `findings_bounced`/`class_instances_found`,
and the verdict's `regression_risks`/`standards_violations`/`overall`/
`reject_reason` into the schemas — that drift is resolved. Remaining drift:
the verifier profile's prose output block still shows fields the committed
verdict schema does not carry in that shape (`standards_violations` semantics
match, but the profile's `reject_reason` prose "required unless accept" is a
WORKFLOW rule here, and its worked examples predate the schema pass) — minor,
to reconcile with the profile author.

Derive-and-check (`overall`): the workflow re-derives the verdict's overall
from `per_finding` (accept iff all `fixed`; reject if any `not_fixed`/
`regression_introduced`; else partial_accept) and REJECTS a verdict whose
asserted value disagrees, or whose non-accept overall lacks a substantive
`reject_reason` — the loop decision flows through the DERIVED value only, and
every disagreement is recorded, cycle-stamped, in the brief result's
`verdict_mismatches`.

The shared test-path rule (gate 1's diff-scope check; gate 2 protects the
full authored set gate 1 admitted under it): a path is a test path iff a
directory component is `tests`/`test`, or the file stem ends in
`_test`/`_tests`, or the file name starts with `test_`; manifest `test_file`
paths are additionally allowed explicitly (`worker/src/handlers.rs
is_test_path`).

## The applier CLI contract (`--kind` per stage artifact)

`ledger_update` invokes the ledger applier — being built in the yggdrasil
repo in parallel — as:

```
python3 scripts/remediation/apply_transitions.py \
    --ledger <ledger_path>        # the in-repo ledger JSON (DECISIONS.md D1)
    --artifact <artifact.json>    # a temp file holding one stage artifact
    --kind test_manifest|fix_report|verdict|disposition
```

run with CWD = `config.repo_root`. Exit 0 = transitions applied; any non-zero
exit = refused, recorded on the brief result as `applied: false` with the
applier's output (never swallowed). The artifact payloads:

- `test_manifest` — `test-manifest.schema.json`.
- `fix_report` — `fix-report.schema.json`.
- `verdict` — `verdict.schema.json`.
- `disposition` — the brief's terminal record, shaped:

```json
{
  "brief_id": "B-1",
  "disposition": "accepted | gate1_failed | cycle_cap_exhausted",
  "fix_cycles": 2,
  "test_edit_attempts": 0,
  "could_not_reproduce": ["YG-367"],
  "detail": "human-readable account"
}
```

The child applies, in order: `test_manifest`, then `fix_report` (when a
developer round ran), then `verdict` (when the verifier ran), then always
`disposition`.

## Prompts

The role DOCTRINE lives in the yggdrasil profiles
(`docs/design/remediation-flow/profiles/*.md`), loaded at worker startup from
`--profiles-dir` (required). Each agent prompt is assembled by ONE dumb
function per role in `worker/src/prompts.rs`:

```
prompt = <profile markdown> + "---" + "## Run context: <role-specific title>" + ```json <activity input JSON> ```
```

The activity input JSON is the workflow's structured context verbatim — for
the test-author it is ALREADY recommendation-free, enforced by the Gleam
codec (`test/codec_test.gleam` proves the codec cannot emit the field). The
prompt interface (sections and order) is the convergence point with the
profile author (DECISIONS.md D3); change it only in `prompts.rs`.

STANDING CONTRACT (design owner, 2026-07-07): the profile markdown reaches
the assembled prompt byte-identical from the checkout — nothing trims,
reflows, or normalizes it beyond removing TRAILING whitespace. Pinned by
`prompts.rs::profile_markdown_survives_byte_identical_up_to_trailing_trim`.

## Wave report skeleton

`remediation_wave` fills the `wave-report.schema.json` metrics its own
artifacts can compute (`fix_cycles_per_brief`, `first_pass_acceptance_rate`,
`could_not_reproduce_rate`, `deviation_count`, `test_edit_attempts`,
`class_siblings_per_brief`) and emits `null` for everything ledger-derived or
later-stage (fail-first validity rates, overturned verdicts, re-audit rates,
flow times, queues). The strict schema types those as numbers: the skeleton
is intentionally NOT schema-complete until the ledger-keeper fills it — a
null is honest where a fabricated zero would read as a measurement.

## Running

```
# Gleam workflow package (this directory)
gleam test

# Rust worker
cd worker
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test

# Worker invocation (liminal server-push transport)
worker/target/debug/remediation-worker \
  --address 127.0.0.1:50061 \
  --repo-root  /path/to/yggdrasil \
  --profiles-dir /path/to/yggdrasil/docs/design/remediation-flow/profiles \
  --norn-bin norn
```

The worker strips `OPENAI_API_KEY` from Norn's child environment (ChatGPT
OAuth login, as pipeline-run does); it holds no secrets. The workspace base
(`/tmp/aion-remediation/ws`) is a shared constant between
`src/remediation_brief.gleam` and `worker/src/handlers.rs` — keep them in
sync.
