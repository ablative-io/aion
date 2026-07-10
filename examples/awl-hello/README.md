# awl-hello — the AWL end-to-end runway

The smallest complete AWL-authored workflow: `awl_hello.awl` (rev-2 surface,
verbatim the spec's worked example 1) is the source of truth, and
`src/awl_hello.gleam` is the module the emitter generates from it — committed
so the example packages and builds without running the toolchain, never
edited by hand. A tiny Rust activity worker (`worker/`) serves the two
activities. Regenerate the module after any `.awl` change:

```
aion awl emit awl_hello.awl --output src/awl_hello.gleam
```

## The workflow

```awl
//! Greet a name, then shout it — the first workflow written in AWL and run for real.
workflow awl_hello
  input name: String
  outcome shouted: type Shouted, route success

type Greeting { greeting: String }
type Shouted  { text: String }

worker awl_hello
  action greet(name: String) -> Greeting
  action shout(text: String) -> Shouted

step greet_and_shout
  name |> greet |> .greeting |> shout |> route shouted
```

One step, one pipe chain: the input threads through `greet`, the `.greeting`
field access, and `shout`, and the piped value becomes the `shouted`
outcome's payload. The run ends as
`{"outcome": "shouted", "payload": {"text": "HELLO, <NAME>!!"}}`
(see `schemas/output.json`).

## Activity contract

Both activities are served by `worker/` on ONE liminal connection:

| Activity | Task queue  | Input                | Output                                      |
|----------|-------------|----------------------|---------------------------------------------|
| `greet`  | `awl_hello` | `{"name": String}`   | `{"greeting": String}` — `Hello, <name>!`   |
| `shout`  | `awl_hello` | `{"text": String}`   | `{"text": String}` — uppercased + `!`       |

The server routes a pushed activity by (namespace, task_queue, node) only —
never by activity type. In rev-2 the `worker awl_hello` block name IS the
task queue, so the generated module pins `task_queue("awl_hello")` on every
call. The actions declare no `node` config, so dispatches are node-unpinned
and match any worker on the queue — including this worker, which registers
node `hello` (an action would add `node hello` on its config line to
require it). The authoritative queue string lives twice and MUST agree:
`TASK_QUEUE` in `worker/src/main.rs`, `worker awl_hello` in `awl_hello.awl`.

## Running it

1. Edit `awl_hello.awl`; `aion awl check awl_hello.awl`, then regenerate
   `src/awl_hello.gleam` with `aion awl emit` (command above).
2. `gleam build` (in this directory).
3. `aion package examples/awl-hello` → `awl-hello.aion` (see
   `workflow.toml`).
4. Deploy the package to a running `aion server`, start the worker
   (`cargo run --manifest-path worker/Cargo.toml -- --address <liminal
   addr>`; `--ready-file <path>` writes a readiness marker on first
   connect), then `aion start awl_hello --input '{"name":"Ada"}'`.
