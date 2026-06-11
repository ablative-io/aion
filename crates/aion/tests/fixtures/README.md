# aion engine integration fixtures

This directory contains deliberately tiny Erlang workflow fixtures used by the
`aion` integration tests. Both the source files and compiled BEAMs are checked
in:

- `aion_fixture_workflow.erl` / `.beam`
- `aion_parent_fixture.erl` / `.beam`
- `aion_parent_query_fixture.erl` / `.beam`
- `aion_child_fixture.erl` / `.beam`
- `aion_collect_fixture.erl` / `.beam`
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
child correlation/replay end-to-end tests in `tests/child_workflows_e2e.rs`.
`await_child/1` returns child success/failure as data with the SDK's
`"ok:"`/`"error:"` payload prefixes; `{error, _}` is reserved for engine
faults:

- `child_round_trip/1` spawns one `aion_child_fixture` child, awaits it, and
  returns `{ChildId, ChildResult}`.
- `child_then_signal/1` does the same but gates completion on a `release`
  signal so tests can restart the engine after the child completed.
- `two_children/1` spawns two children with a `mid` signal consumed between
  the spawns, awaits both, gates on `release`, and returns both child ids.

`aion_parent_query_fixture` exercises the two-phase suspending `await_child`
native for `tests/child_await_e2e.rs`, hand-rolling the same query pump loop
as `aion_fixture_query` so a parent parked inside `await_child` can answer
queries:

- `await_gated/1` spawns one `aion_child_fixture` child, parks in a pumped
  `await_child`, gates on `release`, and returns only the child's result
  (so cross-run history-shape comparisons never embed child ids).
- `queryable_await/1` registers a `state` query handler first, then runs
  `await_gated/1`.

`aion_child_fixture` is the child workflow type those parents spawn:

- `complete/1` returns the known result `42`.
- `wait/1` blocks in `receive` so tests can observe a live child execution.
- `gated/1` parks in the suspending `receive_signal` native until a
  `child_go` signal arrives, then returns `42` — used to hold a parent
  parked in `await_child` for as long as a test needs.
- `can_once/1` continues-as-new once (input `"second"` marks the
  replacement run) and the replacement completes with `42` — used to prove
  `await_child` follows the continue-as-new run chain transparently.

`aion_collect_fixture` exercises the two-phase suspending
`collect_all`/`collect_race` natives for `tests/concurrency_e2e.rs`,
hand-rolling the same query pump loop as `aion_fixture_query`. Activity names
follow the test dispatcher's gate protocol (`gated_ok:K`/`gated_fail:K`
block until the test releases gate `K`, then succeed/fail), and every entry
gates completion on a `release` signal so tests can restart the engine with
the collect's terminals recorded but the run still live:

- `all_two/1` collects two gated activities and returns the result list.
- `all_fail_fast/1` collects one succeeding and one failing activity and
  returns the fail-fast error message as a JSON string.
- `race_two/1` races two gated activities and returns the winner's payload.
- `race_fail/1` races a failing first-settler against a gated success and
  returns the failure message (first-settle semantics) as a JSON string.
- `all_timeout/1` runs a collect of two never-released activities under a
  300 ms `with_timeout` and pins the canonical
  `timeout:deadline expired` scope error.
- `queryable_all/1` registers a `state` query handler first, then runs
  `all_two/1`.

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
- `activity_gated/1` registers a `state` handler, dispatches one `gated_ok:a`
  activity (the test dispatcher blocks until the test releases the gate), and
  parks in the pumped `await_activity_result` yield point, then gates on
  `release` — used to query a workflow parked on an in-flight activity and to
  prove crash/replay byte-identity over the recorded activity terminal.
