# Getting started with Aion

The getting-started guide lives at
[`docs/GETTING-STARTED.md`](docs/GETTING-STARTED.md). It takes you from
nothing to a completed durable workflow using only published artifacts:
install the `aion` CLI, write and package a Gleam workflow, scaffold a Rust
activity worker, run the server, deploy, and operate a run end to end. The
stock binary defaults to the haematite store; libSQL is the alternative
durable backend.

AWL is the other first-class authoring path. Use `aion awl check`,
`aion awl fmt`, `aion awl emit`, and `aion awl schema` while editing a `.awl`
document. `aion deploy <file.awl>` direct-compiles and deploys it;
`aion run <file.awl> --input <json>` compiles, deploys, starts, and awaits the
workflow in one motion.

## Working from this repository instead?

If you have a checkout and want to run the bundled examples from source:

- [`examples/hello-world/README.md`](examples/hello-world/README.md) — the
  first run: build, package, serve, and operate the hello-world workflow.
- [`examples/order-fulfillment/README.md`](examples/order-fulfillment/README.md)
  — the flagship saga (retries, signal/timeout race, child workflow,
  compensation), walked through in
  [`docs/examples/order-saga.md`](docs/examples/order-saga.md).
- [`dev-config.toml`](dev-config.toml) — the local development server
  config; run the server from source with
  `cargo run -p aion-cli -- server --config dev-config.toml`.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — development workflow, lint and test
  gates.
