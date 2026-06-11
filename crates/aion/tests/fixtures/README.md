# aion engine integration fixtures

This directory contains deliberately tiny Erlang workflow fixtures used by the
`aion` integration tests. Both the source files and compiled BEAMs are checked
in:

- `aion_fixture_workflow.erl` / `.beam`
- `aion_parent_fixture.erl` / `.beam`
- `aion_child_fixture.erl` / `.beam`
- `aion_fixture_query.erl` / `.beam`

The test suite loads the committed `.beam` bytes through `RuntimeHandle` and
through `.aion` package loading, so running `cargo test` does **not** require an
Erlang or Gleam toolchain.

To regenerate the BEAMs after editing a source file, run from the repository
root:

```sh
erlc -Werror -o crates/aion/tests/fixtures crates/aion/tests/fixtures/*.erl
```

`aion_fixture_workflow` has no external dependencies. It exports:

- `complete/0`, which returns the known result `42`.
- `wait/0`, which blocks in `receive` so cancellation can observe a live workflow.
- `activity/1`, which blocks in `receive` so dispatch tests can observe a linked
  in-VM activity child before workflow cancellation propagates through links.

`aion_parent_fixture` exercises the child-workflow NIF bridge
(`aion_flow_ffi:spawn_child/3`, `await_child/1`, `receive_signal/2`) for the
child correlation/replay end-to-end tests in `tests/child_workflows_e2e.rs`:

- `child_round_trip/1` spawns one `aion_child_fixture` child, awaits it, and
  returns `{ChildId, ChildResult}`.
- `child_then_signal/1` does the same but gates completion on a `release`
  signal so tests can restart the engine after the child completed.
- `two_children/1` spawns two children with a `mid` signal consumed between
  the spawns, awaits both, gates on `release`, and returns both child ids.

`aion_child_fixture` is the child workflow type those parents spawn:

- `complete/1` returns the known result `42`.
- `wait/1` blocks in `receive` so tests can observe a live child execution.

`aion_fixture_query` exercises the workflow-query yield-point pump protocol
(`aion_flow_ffi:register_query/2`, `reply_query/2`, `reply_query_error/2`,
the `{error, <<"aion_query:", Json/binary>>}` await sentinel, and the
`{aion_query_handler, Name}` process-dictionary handler key) for
`tests/engine_query.rs`. It hand-rolls the pump loop instead of depending on
the `aion_flow` SDK, proving the raw protocol:

- `queryable/1` registers a `state` handler (replies a payload embedding the
  query id), a `boom` handler (raises, proving `HandlerFailed` without a
  process crash), and a `records` handler (calls the recording `send_signal`
  NIF, proving the servicing guard refuses it), then parks on a `release`
  signal behind the pump and returns `42`.
- `staged/1` gates on a `step` signal and then `release`, so restart tests
  have recorded progress before the crash point and replay re-registers the
  handler by re-executing from the top.
- `unpumped/1` registers a handler but parks in a plain Erlang `receive`
  with no pump (query timeout coverage); the raw receive matches the
  engine's signal wake marker, after which a pumped `finish` await discards
  the stale query and completes.
- `busy/1` cycles forty pumped 20 ms sleeps before gating on `release`, so
  queries arrive during active execution and are answered at a sleep entry.
