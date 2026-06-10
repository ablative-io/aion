# Aion

**A durable workflow engine for Gleam, Rust, and the BEAM.**

Aion gives you Temporal-class durable execution — workflows that survive
crashes, replay from event history, durable timers, signals, and child
workflows — built on the BEAM's native process model for concurrency,
supervision, and fault tolerance. It runs on [beamr](https://crates.io/crates/beamr),
a Rust implementation of the BEAM VM.

> Aion is the Greek conception of eternal, unbounded time — distinct from
> Chronos, who is sequential, ticking time. A durable workflow lives in
> Aion's time: it persists across crashes, restarts, and arbitrary delays.
> A workflow that sleeps for three months and resumes is living in eternal
> time, not clock time.

Aion is **general purpose**. The current build includes an embeddable Rust
library, a standalone server, a type-safe Gleam authoring SDK, and in-progress
worker and client SDKs in multiple languages.

## Why Gleam + Rust + BEAM

The three technologies meet at their points of maximum leverage:

- **Gleam** — compile-time type safety for workflow definitions. Activity
  inputs, results, signals, and queries are all statically typed; you
  cannot wire mismatched types together and ship it. Compiles to BEAM
  bytecode.
- **BEAM** (via beamr) — the execution runtime. Designed for systems that
  run forever, handle millions of concurrent processes, recover from
  failure automatically, and upgrade without downtime. Every workflow is a
  process; every activity is a supervised child process.
- **Rust** — the durable substrate. Persistence, the event store, the
  network API, and performance-critical infrastructure.

Temporal had to build distributed process management, supervision, and
fault tolerance from scratch in Go. The BEAM provides them natively. Aion
starts with the runtime Temporal wished it had and adds the durability
layer the BEAM traditionally lacks.

## Architecture

Three layers:

1. **beamr** — the BEAM runtime. Processes, mailboxes, selective receive,
   links, monitors, supervision, timers, hot code loading. (External
   dependency, already built.)
2. **The Aion engine** (`crates/aion`) — implemented workflow lifecycle,
   event-sourced durability and replay, durable timers, signal routing,
   queries, and child workflows. Transport-agnostic.
3. **The SDKs and transports** — the Gleam authoring SDK (`gleam/aion_flow`),
   the Rust NIF helper, the network server, and worker/client SDKs. These are
   usable but still being hardened across languages.

The full vision and the component breakdown live in
[`docs/design/workflow-engine/`](docs/design/workflow-engine/):
- [`DESIGN-OVERVIEW.md`](docs/design/workflow-engine/DESIGN-OVERVIEW.md) —
  the whole-system design.
- [`COMPONENT-ARCHITECTURE.md`](docs/design/workflow-engine/COMPONENT-ARCHITECTURE.md) —
  every crate and package and how they fit.

## Repository layout

```
crates/            Rust crates (the engine, store, package, proto, server, nif, worker, client)
gleam/             Gleam packages (aion_flow authoring SDK, aion_client)
sdks/python/       Python worker + client SDKs
sdks/typescript/   TypeScript worker + client SDKs
apps/              The under-development monitoring dashboard (React + Vite)
conformance/       Cross-language conformance suites
docs/design/       The design — one folder per cluster, plus the overview
tools/             Workspace tooling (scaffold.py)
workspace.json     Machine-readable description of every component
```

### Components

| Crate / package | Role |
|---|---|
| `aion-core` | Domain model: events, payloads, identifiers, status, errors |
| `aion-store` | The `EventStore` contract + in-memory reference + conformance suite |
| `aion-store-libsql` | Default durable store over libSQL (embedded + replica sync) |
| `aion-package` | The `.aion` package format, content-hash versioning |
| `aion` | The engine: lifecycle, supervision, durability/replay, timers, signals, queries |
| `aion-nif` | Rust helper for in-VM activity NIFs |
| `aion-proto` | Shared wire contract (gRPC + serde) |
| `aion-server` | Standalone deployable: HTTP/gRPC/WebSocket + worker protocol |
| `aion-worker` | Rust remote-worker SDK |
| `aion-client` | Rust caller SDK |
| `gleam/aion_flow` | The Gleam authoring SDK |
| `gleam/aion_client` | The Gleam caller SDK |
| `sdks/python/*`, `sdks/typescript/*` | Worker + client SDKs, in progress/hardening |
| `apps/aion-dashboard` | Monitoring UI under development |

## Publishing crates

Aion's Rust crates are published leaf-first so each workspace dependency is
available on crates.io before any crate that depends on it. The publish order is
derived from `cargo metadata --format-version=1 --no-deps` and validated by the
publish script before every run:

1. `aion-core`
2. `aion-store`
3. `aion-store-libsql`
4. `aion-package`
5. `aion-nif`
6. `aion`
7. `aion-proto`
8. `aion-server`
9. `aion-worker`
10. `aion-client`
11. `aion-cli`

Use [`scripts/publish-crates.sh`](scripts/publish-crates.sh) for the full ordered
pass. It requires `cargo` and `jq`; live publication also requires valid crates.io
credentials.

```sh
scripts/publish-crates.sh          # validate the order, then cargo publish --dry-run each crate
scripts/publish-crates.sh --live   # validate the order, then cargo publish each crate for real
```

The default mode is a dry run and stops on the first crate that fails. Do not use
`--live` until the full dry-run pass succeeds in the documented order.

## Status

**The core engine is implemented and functional.**

Implemented in the current build:

- The `aion` engine implements workflow lifecycle, event-sourced durability
  and replay, durable timers, signals, queries, child workflows, and the
  transport-agnostic engine API.
- `aion-server` is a functional standalone server with HTTP/JSON, gRPC,
  WebSocket event streams, package loading, and the remote-worker protocol.
- Three working examples live under [`examples/`](examples/), with
  [`examples/hello-world/`](examples/hello-world/) as the recommended first
  run.

In progress / still maturing:

- Documentation and developer-experience polish are ongoing; start with
  [`GETTING-STARTED.md`](GETTING-STARTED.md), [`CONTRIBUTING.md`](CONTRIBUTING.md),
  [`docs/API.md`](docs/API.md), and
  [`gleam/aion_flow/README.md`](gleam/aion_flow/README.md).
- Worker and client SDK packages exist across Rust, Gleam, Python, and
  TypeScript, but live transport coverage and conformance are still hardening
  around the implemented server and engine surfaces.
- The dashboard/static UI is included and served by the server but remains
  under development; use the CLI or HTTP API to observe workflows for now.

## Scaffolding

The workspace skeleton is generated from a single machine-readable
description:

- [`workspace.json`](workspace.json) — every component: kind, path,
  cluster, dependencies.
- [`tools/scaffold.py`](tools/scaffold.py) — reads `workspace.json` and the
  per-cluster `design.json` `structure` maps, and generates the workspace
  `Cargo.toml`, per-crate manifests, module stubs, and the Gleam / Python /
  TypeScript package manifests.

```sh
python3 tools/scaffold.py            # generate missing files (idempotent)
python3 tools/scaffold.py --dry-run  # preview
python3 tools/scaffold.py --force    # regenerate (overwrites)
```

## License

MIT.
