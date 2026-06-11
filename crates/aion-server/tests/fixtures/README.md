# aion-server integration fixtures

This directory contains a deliberately tiny Erlang parent-workflow fixture used
by the `aion-server` namespace integration tests. Both the source file and
compiled BEAM are checked in:

- `aion_fixture_parent.erl`
- `aion_fixture_parent.beam`

The test suite loads the committed `.beam` bytes through `.aion` package
loading, so running `cargo test` does **not** require an Erlang or Gleam
toolchain. The child workflow it spawns is the `aion_fixture_workflow` fixture
committed under `crates/aion/tests/fixtures/`.

`tests/query_workflow.rs` likewise reuses a committed engine-crate fixture
rather than duplicating it here: `aion_fixture_query.erl`/`.beam` from
`crates/aion/tests/fixtures/` (the hand-rolled query-pump fixture with the
`state`/`boom` handlers and the pump-free `unpumped` timeout entry), included
by relative path exactly as `namespace_restart.rs` includes
`aion_fixture_workflow`.

To regenerate the BEAM after editing the source, run from the repository root:

```sh
erlc -Werror -o crates/aion-server/tests/fixtures crates/aion-server/tests/fixtures/aion_fixture_parent.erl
```

The fixture has no compile-time dependencies. It exports:

- `orchestrate/1`, which spawns one `aion_fixture_workflow` child through the
  engine-registered `aion_flow_ffi:spawn_child/3` NIF, awaits it with
  `aion_flow_ffi:await_child/1`, and returns the known result `42`. Spawn or
  await failures crash via badmatch so the parent records a workflow failure
  instead of silently completing.
