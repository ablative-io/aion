# aion engine integration fixture

This directory contains a deliberately tiny Erlang workflow fixture used by the
`aion` integration tests. Both the source file and compiled BEAM are checked in:

- `aion_fixture_workflow.erl`
- `aion_fixture_workflow.beam`

The test suite loads the committed `.beam` bytes through `RuntimeHandle` and
through `.aion` package loading, so running `cargo test` does **not** require an
Erlang or Gleam toolchain.

To regenerate the BEAM after editing the source, run from the repository root:

```sh
erlc -Werror -o crates/aion/tests/fixtures crates/aion/tests/fixtures/aion_fixture_workflow.erl
```

The fixture has no external dependencies. It exports:

- `complete/0`, which returns the known result `42`.
- `wait/0`, which blocks in `receive` so cancellation can observe a live workflow.
- `activity/1`, which blocks in `receive` so dispatch tests can observe a linked
  in-VM activity child before workflow cancellation propagates through links.
