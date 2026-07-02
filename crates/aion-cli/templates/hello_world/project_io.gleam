//// Boundary types for the {{name}} workflow — the authored source of truth
//// (ADR-014, types-first).
////
//// Declare types only here: `aion generate` derives the JSON codecs
//// (`src/{{name}}_codecs.gleam`) and the emitted `schemas/*.json` artifacts
//// from these types. Edit a type, run `aion generate`, and commit the type
//// with its regenerated artifacts together.

/// The workflow's start input.
pub type Input {
  Input(name: String)
}

/// The workflow's recorded result.
pub type Output {
  Output(greeting: String)
}
