# {{name}}

A durable dev pipeline scaffolded by `aion new` from the `dev-pipeline`
template — brief in, landed on main out:

1. `provision_workspace` — an isolated worktree off the base ref,
2. the `{{name}}_dev` child workflow: a warm build **concurrently** with the
   dev agent (`workflow.all`), then a bounded scoped verify-fix loop,
3. the `{{name}}_gate` child workflow: the authoritative workspace-wide
   check sweep,
4. a bounded human-review loop: `request_review`, then the `review_verdict`
   signal raced against a durable deadline (`workflow.with_timeout`) —
   approve lands, structured change requests resume the same agent session
   and re-gate, reject and timeout are typed failures,
5. `land` — `yg branch merge` into the base ref.

Every loop cap, backoff, and deadline is a **required** input field: the
caller decides, the workflow bakes nothing in. Live `{phase, round}` status
is answered by the `{{name}}_status` and `{{name}}_dev_status` queries,
re-registered at every stage transition.

## Layout

- `workflow.toml` — three `[[workflow]]` entries (parent + two children),
  each independently dispatchable; the parent composes the children with
  `workflow.spawn_and_wait`.
- `schemas/` — six JSON Schemas, one input/output pair per entry. **The
  single source of truth for workflow-level I/O.**
- `src/{{name}}_io.gleam` — types + JSON codecs **generated from the
  schemas** by `aion codegen` (the scaffold already ran it). Do not edit;
  regenerate.
- `src/{{name}}.gleam`, `src/{{name}}_dev.gleam`, `src/{{name}}_gate.gleam`
  — the three workflows.
- `src/{{name}}/` — domain types, codecs, typed activity constructors,
  CLI-shelling local implementations (the test seam), and the typed
  process-runner boundary (+ `src/{{name}}_cli_ffi.erl`, the Erlang port
  runner).
- `test/` — a hermetic behavioral suite: the full pipeline runs in-process
  under the `aion/testing` harness with fake-CLI shims on a private `PATH`.
- `worker/` — the standalone Rust activity worker serving all eight
  activities (its own crate against the published `aion-worker` SDK), with
  wire-compat and shim-driven handler tests.

## Schemas are the source of truth — regenerate, never edit

`src/{{name}}_io.gleam` is generated. After **every** schema edit:

```sh
aion codegen .
gleam build
```

and commit the schema and the regenerated module together. In CI, gate on
drift:

```sh
aion codegen . --check
```

The workflow-level codecs in `src/{{name}}/codecs_workflows.gleam` are built
on the generated module and convert to the domain types in
`src/{{name}}/types.gleam` (see `src/{{name}}/io_convert.gleam`), so a
schema change that alters a wire shape is a compile error — never silent
drift. Typed workflow errors, status replies, and activity payloads are not
dispatch-boundary payloads and keep hand-written codecs.

## Run the hermetic test suite

```sh
gleam test
```

Every test runs the real workflow bodies and the real CLI-shelling activity
implementations; per-test fake `yg`/`norn`/`cargo`/`meridian` scripts on a
private `PATH` intercept at the process boundary and record their argv. No
real CLI install is needed — and a missing CLI with no shim is proven to be
a loud typed failure, never a silent skip.

## Run it live

A live run needs three processes: the server, the activity worker, and the
CLI driving the run.

```sh
# Build and package all three archives.
gleam build
aion package .

# Terminal 1 — the server (state persists in aion.db).
aion server --config aion.toml

# Deploy ALL THREE: a spawned child's workflow type is resolved by entry
# module name, so the children must be deployed alongside the parent.
aion deploy {{name}}.aion
aion deploy {{name}}-dev.aion
aion deploy {{name}}-gate.aion

# Terminal 2 — the worker (its own crate; the endpoint is the server's
# [server] grpc_address). It shells to the real `yg`, `norn`, `cargo`, and
# `meridian` CLIs, so those must be on its PATH — or fake-CLI shims, exactly
# like the test suites use.
cargo run --manifest-path worker/Cargo.toml -- --endpoint http://127.0.0.1:50051

# Terminal 3 — start a run. Every cap, backoff, and deadline is required.
aion start {{name}} --input '{
  "repo_root": "/abs/path/to/repo",
  "brief_id": "brief-7", "reviewers": ["your-member-name"],
  "base_ref": "main",
  "placement": "local", "isolation": "worktree",
  "brief": "Implement the widget",
  "design": "docs/design.md", "checklist": "docs/checklist.md",
  "stories": ["story-1"],
  "verify_fix_cap": 3, "review_cap": 3,
  "round_backoff_ms": 30000, "review_deadline_ms": 86400000
}'

# Watch the phase advance with the workflow id `aion start` printed.
aion query <workflow-id> {{name}}_status

# When the run parks in the review wait, drive the verdict by hand:
aion signal <workflow-id> review_verdict --payload '{"decision":"approve"}'

# Structured change requests and rejections are typed payloads too:
aion signal <workflow-id> review_verdict --payload '{
  "decision": "request_changes",
  "notes": [{"file": "src/lib.rs", "line": 42, "note": "tighten this"}]
}'
aion signal <workflow-id> review_verdict --payload \
  '{"decision":"reject","reason":"wrong architecture"}'

aion describe <workflow-id> --pretty
```

The review wait survives restarts: kill the server mid-wait, start it again
with the same config, and replay resumes the run exactly where it parked.

## The worker

`worker/` serves the eight activity names the three entries declare
(`provision_workspace`, `warm_build`, `dev`, `scoped_checks`, `dev_resume`,
`full_checks`, `request_review`, `land` — `await_verdict` is a signal, not
an activity) and mirrors the local implementations in
`src/{{name}}/locals.gleam` invocation for invocation. Its serde types in
`worker/src/types.rs` are pinned byte-compatible with the Gleam codecs by
`worker/tests/wire_compat.rs`; `worker/tests/handlers_shims.rs` drives every
handler hermetically with fake-CLI shims. Its reconnect budget is
effectively infinite, so it outwaits server restarts.

```sh
cargo test --manifest-path worker/Cargo.toml
```

## Adapting the pipeline

The scaffold shells to `yg` (worktree provisioning, affected-set scoping,
diagnostics checks, and landing via `yg branch merge`), `norn` (the dev
agent, resumed by deterministic session id `{{name}}-<brief_id>`), `cargo`
(the advisory warm build), and `meridian` (review requests:
`meridian review request --reviewer <NAME>... <BRANCH>`, reviewers a
required input field). Swap any of them for your
own tooling in `src/{{name}}/locals.gleam` (the in-process test seam) and
`worker/src/handlers.rs` (the deployed worker) — keep the two mirrored, and
keep the hermetic suites green: they assert the real argv of every step.
