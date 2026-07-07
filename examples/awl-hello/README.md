# awl-hello — the AWL codegen end-to-end runway

The smallest complete landing strip for AWL-generated workflows: a tiny Rust
activity worker plus a Gleam project shell whose one workflow module is a
placeholder that AWL-emitted code will replace. Everything around the
generated module — worker, packaging descriptor, schemas, routing contract —
is already real and gate-clean, so the ONLY moving part in an AWL demo is the
emitted `src/awl_hello.gleam`.

## Activity contract

Both activities are served by `worker/` on ONE liminal connection:

| Activity | Task queue  | Node    | Input                | Output                                      |
|----------|-------------|---------|----------------------|---------------------------------------------|
| `greet`  | `awl_hello` | `hello` | `{"name": String}`   | `{"greeting": String}` — `Hello, <name>!`   |
| `shout`  | `awl_hello` | `hello` | `{"text": String}`   | `{"text": String}` — uppercased + `!`       |

The server routes a pushed activity by (namespace, task_queue, node) only —
never by activity type — so the workflow side must pin both dimensions. The
authoritative strings live twice and MUST agree: `TASK_QUEUE`/`NODE` in
`worker/src/main.rs`, `task_queue`/`hello_node` in `src/awl_hello.gleam`.

The placeholder workflow accepts `{"name": String}`, chains `greet` then
`shout`, and returns the shouted greeting (`"HELLO, <NAME>!!"`).

## Intended flow

1. Author a `.awl` source; the AWL emitter generates a Gleam workflow module.
2. Replace `src/awl_hello.gleam` with the generated module (same module
   name, entry function `run`, and activity contract).
3. `gleam build` (in this directory).
4. `aion package examples/awl-hello` → `awl-hello.aion` (see
   `workflow.toml`).
5. Deploy the package to a running `aion server`, start the worker
   (`cargo run --manifest-path worker/Cargo.toml -- --address <liminal
   addr>`; `--ready-file <path>` writes a readiness marker on first
   connect), then `aion start awl_hello --input '{"name":"Ada"}'`.
