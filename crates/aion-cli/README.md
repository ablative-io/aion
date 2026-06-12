# aion-cli

The Aion command line. The crate is named `aion-cli`; the installed binary
is **`aion`** — the one user-facing binary for running the server, packaging
workflows, deploying packages, and operating workflow executions over gRPC.

## Install

```sh
cargo install aion-cli --locked
```

## Subcommands

- `aion server --config aion.toml` runs the Aion server (the `aion-server`
  crate is the library it embeds). `--workflow-package <path>` preloads
  `.aion` archives at boot.
- `aion package [PATH] [--out <FILE>] [--build]` packages a Gleam workflow
  project into a `.aion` archive. Local-only; never connects to a server.
- `aion deploy <archive>` deploys a `.aion` archive to a running server
  (requires the server's `[deploy]` surface to be enabled).
- `aion versions [--workflow-type <name>]` lists loaded workflow versions
  with routing flags.
- `aion route <workflow-type> <content-hash>` re-points routing to an
  already-loaded version (rollback / roll-forward).
- `aion unload <workflow-type> <content-hash>` unloads a non-routed,
  unpinned version.
- `aion start <workflow-type> --input '<json>'` starts a workflow execution.
- `aion signal <workflow-id> <signal-name> --payload '<json>' [--run-id <id>]`
  sends a signal.
- `aion query <workflow-id> <query-name> [--run-id <id>]` performs a live
  read-only query.
- `aion cancel <workflow-id> [--reason <text>] [--run-id <id>]` requests
  cancellation.
- `aion list [--status <status>]` and
  `aion describe <workflow-id> [--run-id <id>] [--raw]` cover visibility and
  history.

Global flags: `--endpoint` (default `127.0.0.1:50051`), `--namespace`
(default `default`), `--subject` (default `cli-user`), `--token` (overrides
the `AION_TOKEN` environment variable), and `--pretty`.

## Minimal usage

```sh
aion --endpoint 127.0.0.1:50051 \
  start hello_world --input '{"name":"Ada"}'
```

## Error reporting

Every operational failure prints one report to stderr and exits with code 1
(CLI usage mistakes exit 2); stdout stays reserved for the JSON result
document. Failures that carry the client taxonomy render as

```text
error[<class>]: <operation>: <server detail message>
  server error type: <ErrorType>   # when the wire carried one
  hint: <actionable next step>     # for classes with a known remedy
```

where `<class>` is aligned with the wire error codes: `not_found`,
`already_exists`, `query_failed`, `query_timeout`, `unknown_query`,
`not_running`, `cancelled`, `unavailable`, `unauthenticated`,
`namespace_denied`, `invalid_input`, `backend`, and — on the deploy
surface — `deploy_denied` and `version_pinned`. Local failures without a
taxonomy class render their full cause chain on one `error:` line. The full
taxonomy is documented in the repository's
[errors reference](https://github.com/ablative-io/aion/blob/main/docs/errors.md).
