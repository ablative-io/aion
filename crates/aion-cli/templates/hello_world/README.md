# {{name}}

A minimal Aion durable workflow scaffolded by `aion new` from the
`hello-world` template: decode a typed input, return a typed greeting —
start → complete, no activities.

## Layout

- `src/{{name}}.gleam` — the workflow. Edit the typed `handle` function; the
  raw engine plumbing below the generated-code marker decodes/encodes JSON
  for you.
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
aion start {{name}} --input '{"name":"Ada"}'
```

The run completes immediately. Inspect the result with the workflow id that
`aion start` printed:

```sh
aion describe <workflow-id> --pretty
```

The summary shows `"status": "Completed"` and the history carries the result
`{"greeting":"Hello, Ada!"}`.

## Next steps

- Add an activity served by a worker, a durable timer, a signal, or a query:
  start from
  [the workflow authoring guide](https://github.com/ablative-io/aion/blob/main/docs/guides/workflows.md).
- `aion new <name> --template approval-flow` demonstrates signals, timeout
  races, and queries; `--template saga` adds worker-served activities,
  retries, and compensation.
