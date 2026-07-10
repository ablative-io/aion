# AWL worked examples

The golden rev-2 examples live in [`rev2/`](rev2/). They are verbatim from
[`AWL-2-SPEC.md`](../AWL-2-SPEC.md) (the spec of record) and show what a
complete, coherent AWL document looks like — not fragments.

An earlier rev-0/AWL-1 example set that used to sit in this directory
(`about` headers, `output`/`error` declarations, positional `do …(…)` calls,
`as` binders) was written against the pre-rev-2 grammar and has been deleted:
that surface no longer parses. The rev-2 front end is a clean cut with no
compatibility parse; [`AWL-2-BUILD-PLAN.md`](../AWL-2-BUILD-PLAN.md) records
the locked design decisions (D1–D9).

Both examples pass `aion awl check` and are byte-canonical under
`aion awl fmt` (formatting is a no-op), verified against the rev-2 toolchain
on this branch.

## `rev2/awl_hello.awl`

The smallest real workflow: one input, one worker with two actions, one step
whose body is a single pipe chain ending in `route`. This is the document that
ran end-to-end in the first live AWL deployment.

```
$ aion awl check docs/design/aion-authoring/awl/examples/rev2/awl_hello.awl
ok: docs/design/aion-authoring/awl/examples/rev2/awl_hello.awl (1 step)
```

## `rev2/dev_brief.awl`

The flagship: a development brief goes in; an adversarially reviewed branch
comes out. It exercises the wide rev-2 surface in one document:

- `//!` doc header; multiple named `outcome`s with `route success` /
  `route failure`
- schema-door types (`type Brief = schema("schemas/brief.schema.json")`) —
  the JSON Schemas live in [`rev2/schemas/`](rev2/schemas/)
- record types with lists (`[LensVerdict]`), optionals (`String?`), and
  `///` field docs
- a `worker` block of typed `action` contracts with routing config
  (`node`, `timeout`, `retry … every`)
- named-argument calls with `-> binding`
- the step DAG via `after` (including a two-parent join)
- a seeded, bounded `loop … counting … until … max` with outcome arms
  (`when` / `otherwise`) routing forward or to a terminal outcome
- `fork … in … / join ->` fan-out over a runtime list, then a
  `|> filter(.blocking)` pipe and an `is empty` guard deciding the final
  outcome

```
$ aion awl check docs/design/aion-authoring/awl/examples/rev2/dev_brief.awl
ok: docs/design/aion-authoring/awl/examples/rev2/dev_brief.awl (5 steps)
```

Note: `aion awl check` resolves `schema(…)` imports relative to the `.awl`
file, so the command works from any directory.
