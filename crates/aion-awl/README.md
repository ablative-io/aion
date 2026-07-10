# aion-awl

Front end for the AWL workflow language, rev-2 surface: hand-written lexer,
parser, canonical printer (`parse ∘ print = id`; the printer IS the
formatter), typechecker, JSON Schema derivation, and the Gleam-stopgap
emitter.

Spec of record: `docs/design/aion-authoring/awl/AWL-2-SPEC.md`.
CLI surface: `aion awl check | fmt | emit | schema`.
