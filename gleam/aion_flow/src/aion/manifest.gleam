//// Canonical JSON wire form of a package's activity declarations.
////
//// The typed Gleam declaration is the single source of truth (ADR-014). To
//// generate the activity plumbing, `aion generate` runs a small generated
//// export module that calls a package's `manifest()` function and prints
//// `to_json` of the result; the out-of-process Rust generator parses that JSON
//// to drive codegen. This module is the wire boundary between the typed source
//// and the generator — not a second authoring surface: the JSON is derived
//// from the declarations and never hand-written.

import aion/activity.{type Declaration}
import gleam/json
import gleam/list

/// Serialize a package's activity declarations to the canonical JSON array the
/// `aion generate` extractor consumes.
///
/// Each declaration becomes one object carrying its `name`, `tier`, `input`
/// type, and `output` type, in declaration order. Order is load-bearing: it is
/// the order in which the generator emits wrappers, registration entries, and
/// the `workflow.toml` activities list, so a byte-identical round-trip depends
/// on it.
pub fn to_json(declarations: List(Declaration)) -> String {
  declarations
  |> list.map(declaration_to_json)
  |> json.preprocessed_array
  |> json.to_string
}

fn declaration_to_json(declaration: Declaration) -> json.Json {
  json.object([
    #("name", json.string(activity.declaration_name(declaration))),
    #(
      "tier",
      json.string(
        activity.tier_to_string(activity.declaration_tier(declaration)),
      ),
    ),
    #("input", json.string(activity.declaration_input_type(declaration))),
    #("output", json.string(activity.declaration_output_type(declaration))),
  ])
}
