# Aion

**A durable workflow engine for AWL, Gleam, Rust, and the BEAM.**

Aion gives you Temporal-class durable execution: workflows that survive
`kill -9`, replay from event history, sleep durably for months, and resume
exactly where they were. Write workflows in
[AWL](docs/design/aion-authoring/awl/AWL-2-SPEC.md) — a small, checked
language designed for real work — or in type-safe
[Gleam](https://gleam.run). Both compile to BEAM bytecode and execute on
[beamr](https://crates.io/crates/beamr), a Rust implementation of the
BEAM VM, with durable persistence and transport around it.

```sh
cargo install aion-cli --locked   # installs the `aion` binary
```

**Start here:**

- **[AWL quickstart](docs/QUICKSTART-AWL.md)** — write a `.awl` file,
  deploy it, run it. The fastest path to a working durable workflow.
- **[Getting started (Gleam)](docs/GETTING-STARTED.md)** — the full Gleam
  authoring path, with every file inline.

> Aion is the Greek conception of eternal, unbounded time — distinct from
> Chronos, who is sequential, ticking time. A workflow that sleeps for three
> months and resumes is living in eternal time, not clock time.

## What you get

- **Durable execution** — event-sourced histories with deterministic replay.
  Kill the server mid-run; on restart it replays history and the run resumes
  at the same await, without re-executing completed activities.
- **Two authoring paths** — [AWL](docs/design/aion-authoring/awl/AWL-2-SPEC.md),
  a purpose-built language where every construct is checked before anything
  runs (`aion deploy hello.awl`), or Gleam ([`aion_flow`](gleam/aion_flow/))
  for full language power. Both are statically typed end to end.
- **Activities on workers you own** — Rust, Python, and TypeScript worker
  SDKs over a gRPC worker protocol with acks, reconnect, heartbeats, and
  cooperative cancellation.
- **Versioned deploys** — `.aion` packages are content-hash versioned and
  immutable. Deploy at runtime to a live server, route new starts, roll back
  atomically; running workflows keep their pinned version, and
  runtime-deployed packages persist across restarts.
- **One operator surface** — the `aion` CLI runs the server (`aion server`),
  packages workflows, deploys, and operates runs; HTTP/JSON, gRPC, and
  WebSocket event-stream transports, Prometheus metrics, and structured
  audit logs.

## Honest limits

- Single-node: one server process owns the store. There is no clustering.
- Activity retries are workflow-driven today: the SDK carries an explicit
  `RetryPolicy`, but engine-side automatic re-dispatch is not wired up yet —
  workflows drive their own bounded retry loops (the
  [order-saga example](docs/examples/order-saga.md) shows the pattern).
- The ops console UI is under development; the CLI and HTTP API are the
  operating surfaces.
- Cooperative cancellation covers worker shutdown/drain only. There is no
  cancel frame in the worker protocol, so the server cannot cancel a
  specific in-flight activity; the SDK sets the cancellation flag when a
  worker drains or shuts down (local in-flight activities), not in response
  to a server-initiated per-activity cancel.

## Documentation

| Document | What it covers |
|---|---|
| [AWL quickstart](docs/QUICKSTART-AWL.md) | Write an AWL workflow, deploy it, prove it survives a crash |
| [Getting started (Gleam)](docs/GETTING-STARTED.md) | Zero to completed workflow using the Gleam SDK |
| [AWL language reference](docs/design/aion-authoring/awl/AWL-2-SPEC.md) | The full AWL spec: types, expressions, flow control, fork/join, loops |
| [Workflow authoring guide](docs/guides/workflows.md) | The entry contract, determinism rules, timers, signals, queries, children |
| [Activities & workers guide](docs/guides/activities-and-workers.md) | Worker scaffolding, failure classification, retry semantics |
| [Schema codegen guide](docs/guides/codegen.md) | Generate Gleam types + JSON codecs from your schemas; `--check` CI gate; supported subset |
| [Operations guide](docs/guides/operations.md) | Full config reference, deploy/versioning, persistence & recovery, metrics |
| [Errors reference](docs/errors.md) | Every error code and what to do about it |
| [API overview](docs/API.md) | HTTP/JSON, gRPC, WebSocket transports |
| [Packaging reference](docs/packaging.md) | `workflow.toml` and the `.aion` format |
| [Order saga walkthrough](docs/examples/order-saga.md) | The flagship example: retries, timeout races, child workflows, compensation |

## Why AWL + Gleam + Rust + BEAM

- **AWL** — the Aion Work Language. A small, checked language purpose-built
  for defining real work. Every construct is type-checked before anything
  runs; the compiler catches shape mismatches across the
  workflow-to-worker boundary. Write a `.awl` file, deploy it in one
  command.
- **Gleam** — full language power when AWL's vocabulary isn't enough.
  Compile-time type safety for workflow definitions; activity inputs,
  results, signals, and queries are statically typed end to end.
- **BEAM** (via beamr) — the execution runtime: processes, mailboxes,
  selective receive, supervision, hot code loading. Every workflow is a
  process.
- **Rust** — the durable substrate: the event store, replay machinery,
  network APIs, and the VM itself.

Temporal had to build distributed process management, supervision, and fault
tolerance from scratch. The BEAM provides them natively; Aion adds the
durability the BEAM traditionally lacks.

## Architecture

1. **beamr** — the BEAM runtime (external crate).
2. **The Aion engine** (`crates/aion`) — workflow lifecycle, event-sourced
   durability and replay, durable timers, signal routing, queries, child
   workflows. Transport-agnostic.
3. **SDKs and transports** — the Gleam authoring SDK (`gleam/aion_flow`),
   the server (HTTP/gRPC/WebSocket + worker protocol), and worker/client
   SDKs in Rust, Python, and TypeScript.

The whole-system design lives in
[`docs/design/workflow-engine/`](docs/design/workflow-engine/)
([`DESIGN-OVERVIEW.md`](docs/design/workflow-engine/DESIGN-OVERVIEW.md),
[`COMPONENT-ARCHITECTURE.md`](docs/design/workflow-engine/COMPONENT-ARCHITECTURE.md)).

### Components

| Crate / package | Role |
|---|---|
| `aion-core` | Domain model: events, payloads, identifiers, status, errors |
| `aion-store` | The `EventStore` contract + in-memory reference + conformance suite |
| `aion-store-libsql` | Default durable store over libSQL |
| `aion-package` | The `.aion` package format, content-hash versioning |
| `aion` | The engine: lifecycle, durability/replay, timers, signals, queries |
| `aion-nif` | Rust helper for in-VM activity NIFs |
| `aion-proto` | Shared wire contract (gRPC + serde) |
| `aion-server` | Server library: HTTP/gRPC/WebSocket + worker protocol |
| `aion-worker` | Rust remote-worker SDK (library — scaffold your own binary) |
| `aion-client` | Rust caller SDK |
| `aion-awl` | AWL compiler: lexer, parser, typechecker, emitters (Gleam + direct BEAM) |
| `aion-awl-package` | AWL packaging for one-motion deploy (`aion deploy file.awl`) |
| `aion-awl-lsp` | Language server for AWL (hover, go-to-definition, formatting) |
| `aion-cli` | The `aion` binary: server, packaging, deploy, AWL toolchain, workflow operations |
| `gleam/aion_flow` | The Gleam authoring SDK ([Hex](https://hex.pm/packages/aion_flow)) |
| `gleam/aion_client` | The Gleam caller SDK |
| `sdks/python/*`, `sdks/typescript/*` | Worker + client SDKs |
| `apps/aion-ops-console` | Ops console UI under development |

## Repository layout

```
crates/            Rust crates (engine, store, AWL compiler, server, worker, client, cli)
gleam/             Gleam packages (aion_flow authoring SDK, aion_client)
sdks/python/       Python worker + client SDKs
sdks/typescript/   TypeScript worker + client SDKs
apps/              The under-development ops console (React + Vite)
conformance/       Cross-language conformance suites
examples/          Working examples (awl-hello, hello-world, dev-brief, cargo-gates, and more)
editors/           Editor plugins (nvim-awl, zed-awl)
tools/             Workspace tooling (scaffold.py, tree-sitter-awl grammar)
docs/              User documentation; docs/design/ holds the full design
workspace.json     Machine-readable description of every component
```

## Publishing

Rust crates are published leaf-first; the order is derived from
`cargo metadata` and validated by
[`scripts/publish-crates.sh`](scripts/publish-crates.sh) (dry-run by default,
`--live` to publish). The Gleam SDK (`aion_flow`) is published to Hex.
See [`CONTRIBUTING.md`](CONTRIBUTING.md) for development workflow.

## License

AGPL-3.0-only. See [`LICENSE`](LICENSE).

Created by [Tom Whiting](https://github.com/tomWhiting). If you build on
Aion or write about it, a link back here is appreciated.
