//// Tests for the typed activity declaration form (WA-001 R1) and the canonical
//// JSON wire form the `aion generate` extractor consumes.
////
//// These pin the Gleam side of the contract the out-of-process Rust generator
//// parses: the field names, the tier strings, and the declaration order. The
//// generator hard-codes this shape, so a change here is a deliberate
//// cross-language contract change.

import aion/activity
import aion/codec
import aion/manifest
import gleam/dynamic/decode
import gleam/json
import gleeunit/should

fn string_codec() -> codec.Codec(String) {
  codec.json_codec(json.string, decode.string)
}

fn int_codec() -> codec.Codec(Int) {
  codec.json_codec(json.int, decode.int)
}

/// A declaration erases its typed input/output references down to the names and
/// tier the generator reads.
pub fn declaration_erases_to_names_test() {
  let declaration =
    activity.declare(
      "reserve_inventory",
      activity.RemotePython,
      activity.type_ref("OrderInput", string_codec()),
      activity.type_ref("InventoryReservation", int_codec()),
    )

  activity.declaration_name(declaration)
  |> should.equal("reserve_inventory")
  activity.declaration_input_type(declaration)
  |> should.equal("OrderInput")
  activity.declaration_output_type(declaration)
  |> should.equal("InventoryReservation")
  activity.declaration_tier(declaration)
  |> should.equal(activity.RemotePython)
}

/// Every tier renders to its canonical wire string.
pub fn tier_strings_test() {
  activity.tier_to_string(activity.InVm)
  |> should.equal("in_vm")
  activity.tier_to_string(activity.RemotePython)
  |> should.equal("remote_python")
  activity.tier_to_string(activity.RemoteRust)
  |> should.equal("remote_rust")
}

/// Declarations with *different* input and output types live together in one
/// `List(Declaration)` — the erasure that makes a package-wide manifest
/// possible — and serialize to the canonical JSON array in declaration order.
pub fn manifest_to_json_pins_wire_format_test() {
  let declarations = [
    activity.declare(
      "reserve_inventory",
      activity.RemotePython,
      activity.type_ref("OrderInput", string_codec()),
      activity.type_ref("InventoryReservation", int_codec()),
    ),
    activity.declare(
      "ship_order",
      activity.InVm,
      activity.type_ref("OrderInput", string_codec()),
      activity.type_ref("Shipment", string_codec()),
    ),
  ]

  manifest.to_json(declarations)
  |> should.equal(
    "[{\"name\":\"reserve_inventory\",\"tier\":\"remote_python\",\"input\":\"OrderInput\",\"output\":\"InventoryReservation\"},{\"name\":\"ship_order\",\"tier\":\"in_vm\",\"input\":\"OrderInput\",\"output\":\"Shipment\"}]",
  )
}

/// The empty manifest serializes to an empty JSON array.
pub fn manifest_to_json_empty_test() {
  manifest.to_json([])
  |> should.equal("[]")
}
