# Getting started with Aion

Two paths, depending on how you want to author workflows:

## AWL — the fast path

Write workflows in AWL, a small checked language built for Aion. No Gleam
toolchain needed for authoring.

**→ [AWL quickstart](docs/QUICKSTART-AWL.md)** — write a `.awl` file,
deploy it, run it, prove it survives a crash.

## Gleam — the full-power path

Write workflows directly in Gleam for full language control. Every file
inline, every step explained.

**→ [Getting started (Gleam)](docs/GETTING-STARTED.md)** — zero to a
completed durable workflow using only published artifacts.

## Working from this repository instead?

If you have a checkout and want to run the bundled examples from source:

- [`examples/awl-hello/README.md`](examples/awl-hello/README.md) — the
  smallest AWL workflow, end to end.
- [`examples/hello-world/README.md`](examples/hello-world/README.md) — the
  Gleam hello-world: build, package, serve, and operate.
- [`examples/order-fulfillment/README.md`](examples/order-fulfillment/README.md)
  — the flagship saga (retries, signal/timeout race, child workflow,
  compensation), walked through in
  [`docs/examples/order-saga.md`](docs/examples/order-saga.md).
- [`dev-config.toml`](dev-config.toml) — the local development server
  config; run the server from source with
  `cargo run -p aion-cli -- server --config dev-config.toml`.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — development workflow, lint and test
  gates.
