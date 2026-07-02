# {{name}}

A durable agent loop scaffolded by `aion new` from the `agent` template:

1. **scout -> act -> verify** — three parameterised agent steps, each a
   worker-served activity driven by a prompt from the start input. The
   scaffold bundles no agent runtime; the worker (`worker/`) decides what
   each step does, so a new agentic family is configuration, not code.
2. **signal-gated human review** — a durable `workflow.receive` raced against
   a caller-chosen deadline. The run suspends — for seconds or weeks — and
   survives server restarts while it waits. An `agent_review` signal carrying
   `approve` applies the artifact; `reject` or the deadline lapsing holds it.
3. **`agent_status` query** — answerable at every stage.

No deadline is invented: the per-step agent activities run unbounded until the
worker answers, and the human-review wait uses the `review_timeout_ms` you
pass in the start input.

## Layout

- `src/{{name}}.gleam` — the workflow. Edit the typed `handle` function and
  its helpers; the raw engine plumbing below the generated-code marker
  decodes/encodes JSON for you.
- `src/{{name}}_io.gleam` — the authored boundary types (the source of
  truth, types-first). Edit a type, then run `aion generate .`.
- `src/{{name}}_codecs.gleam` — JSON codecs generated from the types by
  `aion generate`. Do not edit; regenerate.
- `workflow.toml` — packaging descriptor read by `aion package`.
- `schemas/` — JSON Schema artifacts emitted from the types by
  `aion generate`. Do not edit; regenerate.
- `aion.toml` — development server configuration.
- `worker/` — the Rust agent-step worker (present when scaffolded with
  `--worker rust`). It serves `scout`, `act`, and `verify`; its
  `StepInput`/`StepOutput` structs mirror the types in
  `src/{{name}}_io.gleam`. Replace its handler bodies with your own agent
  driver.

## Run it

Build and package the workflow:

```sh
gleam build
aion package .
```

Start the server (terminal 1; state persists in `aion.db`):

```sh
aion server --config aion.toml
```

Start the agent-step worker (terminal 2). If this project was scaffolded with
`--worker rust`:

```sh
cargo run --manifest-path worker/Cargo.toml
```

Otherwise write one against a worker SDK — see the
[activities and workers guide](https://github.com/ablative-io/aion/blob/main/docs/guides/activities-and-workers.md).
The workflow dispatches `scout`, `act`, and `verify` on the `default` task
queue.

Deploy and start a run (terminal 3):

```sh
aion deploy {{name}}.aion
aion start {{name}} --input '{"task_id":"task-1","scout_prompt":"survey","act_prompt":"do","verify_prompt":"check","review_timeout_ms":3600000}'
```

The worker runs scout -> act -> verify, then the run suspends waiting for the
review decision. Watch it move with the workflow id that `aion start` printed:

```sh
aion query <workflow-id> agent_status
```

You should see `"stage": "awaiting_review"`. The wait survives restarts: kill
the server, start it again with the same config, and replay resumes the run
exactly where it was without re-running the agent steps.

Approve to apply the artifact:

```sh
aion signal <workflow-id> agent_review --payload '{"decision":"approve","reviewer":"ada"}'
aion describe <workflow-id> --pretty
```

`describe` shows `"status": "Completed"` with `"disposition": "applied"`. Send
`{"decision":"reject","reviewer":"ada"}` instead — or let the deadline lapse —
to hold the artifact: the run completes with `"disposition": "held"`. A held
artifact is a successful, fully recorded run, ready for a human follow-up.

## Change the contract

The boundary types in `src/{{name}}_io.gleam` are the single source of truth
for everything wire-shaped. To evolve the loop — extra prompt fields, richer
step outputs, more review metadata:

1. edit the type in `src/{{name}}_io.gleam`,
2. run `aion generate .` (regenerates `src/{{name}}_codecs.gleam` and
   `schemas/*.json`; `aion generate . --check` verifies a clean tree),
3. update the worker's mirrored structs in `worker/src/main.rs`,
4. commit the type with its regenerated artifacts together.

## Next steps

- Replace the `scout`/`act`/`verify` handler bodies in `worker/src/main.rs`
  with your own agent driver — an LLM call, a tool loop, a `norn` agent —
  keeping the `StepInput` -> `StepOutput` contract.
- [Workflow authoring guide](https://github.com/ablative-io/aion/blob/main/docs/guides/workflows.md)
  — child workflows, timers, determinism rules.
