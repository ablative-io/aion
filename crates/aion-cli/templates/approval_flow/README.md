# {{name}}

A human-in-the-loop approval workflow scaffolded by `aion new` from the
`approval-flow` template: an `approval_decision` signal raced against a
durable deadline with `workflow.with_timeout`, a `status` query re-registered
at every stage, and all outcome paths (approved, rejected, timed out).

## Layout

- `src/{{name}}.gleam` — the workflow. Edit the typed `handle` function and
  its helpers; the raw engine plumbing below the generated-code marker
  decodes/encodes JSON for you.
- `workflow.toml` — packaging descriptor read by `aion package`.
- `schemas/` — JSON Schemas for the workflow input and output.
- `aion.toml` — development server configuration.

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

Deploy and start a run (terminal 2):

```sh
aion deploy {{name}}.aion
aion start {{name}} --input '{"request_id":"req-1","timeout_seconds":3600}'
```

The run suspends durably, waiting for the decision. Ask it where it is with
the workflow id that `aion start` printed:

```sh
aion query <workflow-id> status
```

You should see `"awaiting_approval"`. The wait survives restarts: kill the
server, start it again with the same config, and the run is still waiting.

Deliver the decision — or let the deadline lapse to take the `timed_out`
path:

```sh
aion signal <workflow-id> approval_decision --payload '{"decision":"approved","approver":"ada"}'
aion describe <workflow-id> --pretty
```

`describe` shows `"status": "Completed"` with the result
`{"request_id":"req-1","decision":"approved","decided_by":"ada"}`. Send
`{"decision":"rejected",...}` instead to take the rejection path.

## Next steps

- Add worker-served activities to act on the decision:
  `aion new <name> --template saga` shows activities, workflow-driven
  retries, and saga compensation.
- [Workflow authoring guide](https://github.com/ablative-io/aion/blob/main/docs/guides/workflows.md)
  — timers, queries, signals, child workflows, determinism rules.
