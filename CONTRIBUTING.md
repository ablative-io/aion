# Contributing to Aion

## Prerequisites

- **Rust** — stable toolchain via [rustup](https://rustup.rs)
- **Gleam** — [install](https://gleam.run/getting-started/installing/)
  with Erlang/OTP on your `PATH` (needed for the `aion_flow` SDK and
  examples)
- **Protobuf compiler** — `protoc` for gRPC codegen (`brew install protobuf`
  on macOS)

## Build and test

```sh
cargo build --workspace
cargo test --workspace
```

## Gates

Every commit must pass all three:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Clippy is strict: pedantic lints, `unsafe_code = "deny"`,
`unwrap_used`/`expect_used`/`panic`/`todo` warned. Fix the code — never
silence with `#[allow]`.

## Running from source

Start the server with the bundled dev config:

```sh
cargo run -p aion-cli -- server --config dev-config.toml
```

Run the AWL toolchain:

```sh
cargo run -p aion-cli -- awl check examples/awl-hello/awl_hello.awl
cargo run -p aion-cli -- awl fmt examples/awl-hello/awl_hello.awl
cargo run -p aion-cli -- awl emit examples/awl-hello/awl_hello.awl
```

## Examples

Runnable examples live under `examples/`. Each has its own README:

- [`awl-hello`](examples/awl-hello/) — the smallest AWL workflow
- [`hello-world`](examples/hello-world/) — the Gleam hello-world
- [`cargo-gates`](examples/cargo-gates/) — parallel CI gates in AWL
- [`dev-brief`](examples/dev-brief/) — full dev pipeline with review loops
- [`order-fulfillment`](examples/order-fulfillment/) — the flagship saga

## AWL development

The AWL compiler lives in `crates/aion-awl/`. The toolchain:

```
lexer → parser → canonical printer → typechecker → emitter → packager
```

The LSP server is `crates/aion-awl-lsp/`. Editor plugins:

- **Neovim**: `editors/nvim-awl/`
- **Zed**: `editors/zed-awl/`
- **Tree-sitter grammar**: `tools/tree-sitter-awl/`

## Local workflow artifacts

Some development tools create local process state. These files are
gitignored:

- `.yggdrasil-worktrees/` — temporary workflow worktrees
- `.meridian/` — local Meridian/Norn state
- `.claude/` — local Claude Code settings
- `server.log` — local server output
- `.commit-msg.tmp` — temporary commit-message scratch

## Code standards

This codebase runs mission-critical infrastructure. See
[`CLAUDE.md`](CLAUDE.md) for the full coding standards, load-bearing
invariants, and error handling conventions.

## License

AGPL-3.0-only. By contributing, you agree that your contributions will be
licensed under the same terms.
