//// Round-trip tests for the brief document and resolved-context codecs.
////
//// The primary fixture is the SEEDED brief at
//// `docs/design/brief-dev/briefs/BD-001.json` — a real authored contract
//// (P4), read relative to the package root and never written to. The
//// round-trip guarantee under test is lossless-and-deterministic: decode
//// loses nothing, encode is canonical and stable (decode → encode → decode
//// yields an equal value and a byte-identical re-encode).

import aion/codec
import gleam/json
import gleam/list
import gleam/option
import gleam/string
import gleeunit/should
import stacked_dev/codecs_brief
import stacked_dev/codecs_brief_blocks as blocks
import stacked_dev/types.{
  BriefDocument, BriefRequirement, DevBlock, Implemented, RequirementFiles,
  ResolvedAdr, ResolvedContext, ResolvedItem, ResolvedProvenance,
}
import support/fixtures

const fixture_path = "../../docs/design/brief-dev/briefs/BD-001.json"

fn decoded_fixture() -> types.BriefDocument {
  let assert Ok(raw) = fixtures.read_file(fixture_path)
  let brief_codec = codecs_brief.brief_document_codec()
  let assert Ok(document) = brief_codec.decode(raw)
  document
}

pub fn fixture_decodes_to_the_authored_brief_test() {
  let document = decoded_fixture()
  document.id |> should.equal("BD-001")
  document.cluster |> should.equal("brief-dev")
  list.length(document.requirements) |> should.equal(6)
  document.execution |> should.equal(option.None)
  list.all(document.requirements, fn(requirement) {
    requirement.scout == option.None
    && requirement.dev == option.None
    && requirement.review == option.None
  })
  |> should.be_true
}

pub fn fixture_round_trip_is_lossless_test() {
  let document = decoded_fixture()
  let brief_codec = codecs_brief.brief_document_codec()
  let assert Ok(redecoded) = brief_codec.decode(brief_codec.encode(document))
  redecoded |> should.equal(document)
}

pub fn fixture_encoding_is_deterministic_test() {
  let document = decoded_fixture()
  let brief_codec = codecs_brief.brief_document_codec()
  let encoded = brief_codec.encode(document)
  let assert Ok(redecoded) = brief_codec.decode(encoded)
  brief_codec.encode(redecoded) |> should.equal(encoded)
}

pub fn missing_required_authored_field_fails_loudly_test() {
  let brief_codec = codecs_brief.brief_document_codec()
  let assert Error(codec.DecodeError(reason: _, path: path)) =
    brief_codec.decode("{\"id\":\"BD-001\"}")
  path |> should.equal(["cluster"])
}

pub fn absent_enrichment_blocks_are_absent_from_the_wire_test() {
  let document =
    BriefDocument(
      id: "BD-900",
      cluster: "brief-dev",
      title: "Encoding test",
      depends_on: [],
      blocked_by: [],
      checklist: ["C1"],
      stories: ["S5"],
      design_anchor: ["ADR-007"],
      purpose: "Prove None blocks vanish from the wire.",
      task: "Encode and inspect.",
      requirements: [
        BriefRequirement(
          id: "R1",
          title: "Only authored fields",
          spec: "Plain.",
          acceptance: ["No enrichment keys appear."],
          files: RequirementFiles(create: [], modify: [], delete: []),
          checklist: ["C1"],
          stories: ["S5"],
          scout: option.None,
          dev: option.None,
          review: option.None,
        ),
      ],
      boundaries: [],
      verification: [],
      execution: option.None,
    )
  let encoded = codecs_brief.brief_document_codec().encode(document)
  string.contains(encoded, "\"scout\"") |> should.be_false
  string.contains(encoded, "\"dev\"") |> should.be_false
  string.contains(encoded, "\"review\"") |> should.be_false
  string.contains(encoded, "\"execution\"") |> should.be_false
}

pub fn dev_status_uses_exact_wire_strings_test() {
  let block =
    DevBlock(
      status: Implemented,
      files_changed: [],
      how: "Implemented per the scouted plan.",
      deviation: "",
      checklist: [],
      stories: [],
    )
  let encoded = json.to_string(blocks.dev_block_to_json(block))
  string.contains(encoded, "\"status\":\"implemented\"") |> should.be_true
  json.parse(encoded, blocks.dev_block_decoder()) |> should.equal(Ok(block))
}

pub fn dev_status_rejects_strings_outside_the_enum_test() {
  json.parse("\"half-done\"", blocks.dev_status_decoder()) |> should.be_error
}

pub fn resolved_context_round_trips_test() {
  let context =
    ResolvedContext(
      adrs: [
        ResolvedAdr(
          id: "ADR-003",
          title: "No default timeouts",
          decision: "Caps, backoffs, and deadlines are required inputs; "
            <> "nothing at the aion layer invents a bound.",
          quote: "we shouldn't have a default timeout, the agent steps can "
            <> "take well over an hour",
          decided_by: "Tom",
        ),
      ],
      checklist: [
        ResolvedItem(
          id: "C1",
          text: "Stage-contract schemas copied into the package and drift-gated.",
        ),
      ],
      stories: [
        ResolvedItem(
          id: "S5",
          text: "Stage payload codecs are generated from the canon schemas.",
        ),
      ],
      constraints: [
        ResolvedItem(
          id: "CN7",
          text: "Package stage-contract schemas are byte-identical to canon.",
        ),
      ],
      intention: "Design-system v2 briefs become executable.",
      design_path: "docs/design/brief-dev/design.json",
      provenance: ResolvedProvenance(
        requested_by: "Tom",
        quote: "do this roadmap item",
      ),
    )
  let context_codec = codecs_brief.resolved_context_codec()
  let assert Ok(redecoded) = context_codec.decode(context_codec.encode(context))
  redecoded |> should.equal(context)
}
