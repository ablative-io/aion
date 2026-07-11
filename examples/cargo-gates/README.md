# cargo-gates

`cargo-gates` is an AWL-authored workflow that fans `cargo check`, Clippy,
tests, and the non-mutating formatter check out over one Rust workspace. It
returns each command's real exit code and only reports `clean` when all four
commands pass. Worker output is bounded to the final 40 combined output lines.

`cargo_gates.awl` is copied verbatim from the marked exam artifact at
`docs/design/aion-authoring/awl/exam/workbench/cargo_gates.awl`; that workbench
file is the source of record. `src/cargo_gates.gleam` is generated and committed,
never edited by hand. Regenerate it with:

```sh
~/.cargo/bin/aion awl emit cargo_gates.awl --output src/cargo_gates.gleam
```

The activity worker follows the repository's established `aion-worker` SDK
pattern. It guards the requested path, runs each Cargo process in the blocking
pool, and registers all four actions on the `cargo_gates` task queue.

## Build and package

From this directory:

```sh
gleam build
~/.cargo/bin/aion package --build .
cargo build --manifest-path worker/Cargo.toml
```

The package command writes `cargo-gates.aion`. The worker is a separate binary;
the workflow archive and worker must both be available to run the workflow.

## Deploy and run

The following commands are for an operator with an Aion server and liminal
worker endpoint. This example's build does not run them automatically:

```sh
~/.cargo/bin/aion deploy cargo-gates.aion
cargo run --manifest-path worker/Cargo.toml -- --address 127.0.0.1:50061
~/.cargo/bin/aion start cargo_gates --input '{"workspace_path":"/absolute/path/to/rust-workspace"}'
```

Start the worker in a separate terminal and adjust the liminal address to the
server's configured address. The workspace must exist and contain `Cargo.toml`;
otherwise every action returns a typed failed result with exit code `-1`.
`run_tests` is a real `cargo test --workspace`, so on a large workspace the run
takes as long as that workspace's tests do.
