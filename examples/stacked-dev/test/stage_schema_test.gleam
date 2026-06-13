//// Pin the inline `--output-schema` constants in `stacked_dev/locals` to the
//// drift-gated package stage-contract schemas (BD-003 R4).
////
//// Each Gleam string constant the norn-backed locals pass to
//// `--output-schema` must parse as JSON and be structurally equal — Erlang
//// term equality of the parsed `Dynamic` values, which is insensitive to
//// whitespace and key formatting but exact on structure and values — to the
//// package schema read from the package root. This keeps the inline
//// constants honest against `schemas/scout_report.json`,
//// `schemas/dev_report.json`, and `schemas/review_report.json`, which the
//// drift gate in turn pins byte-for-byte to the design-system canon (CN7).

import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/json
import gleeunit/should
import stacked_dev/locals
import support/fixtures

fn parse(raw: String) -> Dynamic {
  let assert Ok(value) = json.parse(raw, decode.dynamic)
  value
}

fn schema_file(path: String) -> Dynamic {
  let assert Ok(raw) = fixtures.read_file(path)
  parse(raw)
}

pub fn scout_output_schema_matches_the_package_schema_test() {
  parse(locals.scout_output_schema)
  |> should.equal(schema_file("schemas/scout_report.json"))
}

pub fn dev_output_schema_matches_the_package_schema_test() {
  parse(locals.dev_output_schema)
  |> should.equal(schema_file("schemas/dev_report.json"))
}

pub fn review_output_schema_matches_the_package_schema_test() {
  parse(locals.review_output_schema)
  |> should.equal(schema_file("schemas/review_report.json"))
}
