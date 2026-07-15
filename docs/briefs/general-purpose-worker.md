# Brief: general-purpose worker (`run_agent` / `run_command` / `parse_output`)

- **id:** GPW-1
- **status:** ready to implement
- **prerequisite reading:** `docs/WORKER-AUTHORING.md`, then the reference
  implementation `examples/dev-brief/worker/` (the guide explains which files
  demonstrate which pattern).

## Objective

One worker binary providing three general activities so a workflow document
can define agent roles, shell commands, and output parsing **at call time** —
instructions, output schemas, and session identity all become activity input
data instead of compiled worker configuration. The operator can then author
and iterate on agent workflows in the studio without writing a new worker per
experiment.

## Context and decided design

The dev-brief worker bakes its two roles (developer, reviewer) into a role
table: profile markdown loaded at startup, fixed output schemas, fixed session
suffixes. This worker inverts that: **everything role-specific arrives in the
activity input.**

Decisions already made with the operator (do not relitigate):

1. Three activities, no more: `run_agent`, `run_command`, `parse_output`.
2. `run_agent`'s output schema is supplied per call **as a JSON Schema
   string** in the input. A future compiler feature will derive that string
   from the action's declared AWL return type; this worker's contract
   (schema-as-data in the input) is identical before and after that feature,
   so build for schema-as-string now.
3. `parse_output` is pure and mechanical — no agent involved. "Smart"
   extraction is just another `run_agent` call.
4. No mechanical post-run git commit machinery in v1 (dev-brief's
   `PostRunCommit` seam) — see scope out.

## The three activities

### `run_agent` (driven agent, its own node: `agent`)

Input (all per-call):

| field | type | notes |
|---|---|---|
| `instructions` | string, required | appended system prompt (Norn `--append-system-prompt` — NEVER `--system-prompt`) |
| `prompt` | string, required | the per-turn prompt |
| `output_schema` | string, required | a JSON Schema document, passed to Norn `--output-schema` |
| `session_key` | string, optional | Norn `--session-id`. Caller-supplied so e.g. `plan` and `merge` calls can resume ONE coordinator session. Default when absent: `{workflow_id}-agent`. Always pair with `--resume-if-exists`. |
| `workspace_path` | string, required | per-run `--workspace-root` (the dev-brief per-run seam, reused) |
| `disallowed_tools` | string, optional | comma-separated Norn tool deny-list (e.g. `write,edit,apply_patch` for a read-only reviewer) |

Output: the agent's schema-conforming JSON result, returned **verbatim** as
the activity result. Norn validates against the supplied schema; the worker
does not re-validate and does not reshape. The calling document declares
whatever return type it expects — the schema string and the declared type
must agree (the operator owns that agreement until the compiler derives both
from one type).

Implementation shape: dev-brief's `ProfiledNornHarness` wrapper pattern, but
the intercepted seam now reads instructions/schema/session/workspace from the
input instead of role config. The schema must be delivered per run (dev-brief
passes it as a static startup argument — this worker cannot; if Norn requires
a file path for `--output-schema`, write the schema to a per-run file under a
durable worker work dir, never `/tmp`). Env hygiene identical to dev-brief:
`OPENAI_API_KEY` removed from the child env, no secrets touched, `--fast`,
`--reasoning-effort high`, intervention capabilities `InjectMessage` +
`Cancel`.

**Verify, don't assume:** what Norn does with `--append-system-prompt` when a
session is RESUMED (applied once at creation? re-applied per resume?). Test
it against the real binary, document the answer in the worker's README, and
make the harness behavior match the finding.

### `run_command` (shell node: `shell`)

Input: `{ workspace_path: string, name: string, argv: [string] }` — `argv[0]`
is the executable, resolved on `PATH`; `name` labels the run in results.

Output: `{ name, argv, exit_code, passed (exit_code == 0), stdout,
output (combined stdout+stderr), duration_ms }`.

Implementation: dev-brief's `Shell` boundary and gate-battery core, minus the
gate-only extras (no `{base_commit}` substitution, no formatter-normalization
commit, no re-run semantics). The failure taxonomy is law: cannot-run →
terminal `ActivityFailure`; non-zero exit → **successful result carrying the
verdict as data**. Clip `output`/`stdout` to event-sized tails with the
shared `clip` helper (record that clipping in the output as dev-brief does).

### `parse_output` (same shell node, pure — no `Shell` needed)

Input: `{ text: string, mode: string, query: string }` with modes:

- `json_path` — `query` is a dotted path (`result.items.0.name`) into `text`
  parsed as JSON; returns the addressed value (JSON-encoded string if
  non-scalar).
- `regex` — `query` is a regex; returns the first match's capture groups
  (named groups as a map, else positional list).
- `lines` — `query` is a substring filter; returns matching lines.

Output: `{ ok: bool, value: string, error: string }` — a parse miss or bad
query is **returned data** (`ok: false`, `error` says exactly what failed),
never an activity failure, so documents can branch on it. Deterministic:
same input, same output, always — this is what makes it replay-safe.

## Pointers

- `docs/WORKER-AUTHORING.md` — the guide; follow its checklist.
- `examples/dev-brief/worker/src/main.rs` — composition root, role table,
  connection-per-node topology, `serve_with_redial`, `WorkerConfig`.
- `examples/dev-brief/worker/src/harness.rs` — the per-run input-interception
  seam to generalize.
- `examples/dev-brief/worker/src/shell.rs` + `src/handlers/gates.rs` — the
  shell boundary and the battery core `run_command` distills.
- `examples/dev-brief/worker/src/handlers/support.rs` — `clip`.
- `examples/dev-brief/worker/tests/` — the hermetic shim test pattern.
- `examples/dev-brief/awl/dev_brief.awl` — how documents declare workers,
  actions, and nodes.

## Scope in

- New standalone package `examples/general-worker/` (worker crate modeled on
  dev-brief's layout: thin `main.rs`, everything testable in the lib).
- Task queue `general`, two connections/nodes: `agent` (run_agent) and
  `shell` (run_command + parse_output).
- Hermetic tests per the guide: harness argv assembly (instructions, schema
  delivery, session key default vs supplied, workspace root, deny-list),
  run_command taxonomy (unrunnable → terminal; non-zero → data), every
  parse_output mode including malformed input (`ok:false` paths).
- One example `.awl` document under `examples/general-worker/awl/` using all
  three actions, passing `aion awl check`.
- A README documenting each activity's input/output contract, the session-key
  mechanic, the resumed-session `--append-system-prompt` finding, and the
  run_command trust caveat.

## Scope out (explicit)

- Compiler changes (deriving schemas from AWL return types) — queued
  separately; do not start it.
- Mechanical git commit machinery (`PostRunCommit`) — v1 agents own their
  worktree discipline via instructions.
- Sandboxing/allowlisting `run_command` — it is arbitrary execution as the
  worker's user, for the operator's own machine; note the caveat in the
  README, add nothing.
- Any change to the dev-brief example.
- Any launchd unit. Manual start only.

## Acceptance

1. `cargo build`, `cargo clippy` (workspace-law clean), `cargo test`,
   `cargo fmt` all green in the package — full output to files, exit-code
   manifest committed alongside (no piping cargo output through
   grep/head/tail).
2. Hermetic test evidence for every contract row above — each test fails if
   its feature is broken (no vacuous tests).
3. The example document passes `aion awl check` against the built CLI.
4. The resumed-session `--append-system-prompt` behavior is tested against
   the real `norn` binary once, and the finding is written in the README.
5. House laws hold: no `unwrap`/`expect`/`panic` (tests included), no
   `#[allow]`/`#[expect]`/`#[ignore]`, files ≤ 500 code lines, `mod.rs`
   re-exports only, doc-comment identifiers backticked, nothing written to
   `/tmp`, no secrets read or printed.

## Notes

- Commit in small, explicit-path commits (never `git add -A`).
- If a decision point arises that this brief does not cover, record it as an
  open question in the README rather than inventing semantics — the operator
  decides.
