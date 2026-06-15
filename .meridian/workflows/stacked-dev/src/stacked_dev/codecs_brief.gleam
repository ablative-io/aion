//// Hand-written JSON codecs for the v2 brief document and the pre-resolved
//// reference context.
////
//// Hand-written (not generated) because the brief document is not a package
//// dispatch schema — its canon lives in the design system at
//// `docs/design-system/schemas/brief.schema.json`. Wire keys match the
//// schema property names exactly; the optional enrichment keys (scout, dev,
//// review per requirement; execution per brief) are omitted from the wire
//// entirely when the value is `None`. Decoding fails on any missing
//// required authored field — emptiness is authored, never defaulted
//// (ADR-001) — and rejects enum strings outside the schema's sets. The
//// enrichment-block codecs live in `stacked_dev/codecs_brief_blocks`.

import aion/codec
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option
import stacked_dev/codecs_brief_blocks as blocks
import stacked_dev/types.{
  type BriefDocument, type BriefRequirement, type RequirementFiles,
  type ResolvedAdr, type ResolvedContext, type ResolvedItem,
  type ResolvedProvenance, BriefDocument, BriefRequirement, RequirementFiles,
  ResolvedAdr, ResolvedContext, ResolvedItem, ResolvedProvenance,
}

/// Codec for a whole brief document. Encoding is canonical and stable:
/// decode → encode → decode yields an equal value and a byte-identical
/// re-encode (lossless and deterministic; reproducing on-disk pretty
/// formatting is not a goal).
pub fn brief_document_codec() -> codec.Codec(BriefDocument) {
  codec.json_codec(brief_document_to_json, brief_document_decoder())
}

/// JSON encoder for a brief document.
pub fn brief_document_to_json(document: BriefDocument) -> json.Json {
  json.object(list.append(
    [
      #("id", json.string(document.id)),
      #("cluster", json.string(document.cluster)),
      #("title", json.string(document.title)),
      #("depends_on", json.array(document.depends_on, json.string)),
      #("blocked_by", json.array(document.blocked_by, json.string)),
      #("checklist", json.array(document.checklist, json.string)),
      #("stories", json.array(document.stories, json.string)),
      #("design_anchor", json.array(document.design_anchor, json.string)),
      #("purpose", json.string(document.purpose)),
      #("task", json.string(document.task)),
      #(
        "requirements",
        json.array(document.requirements, brief_requirement_to_json),
      ),
      #("boundaries", json.array(document.boundaries, json.string)),
      #("verification", json.array(document.verification, json.string)),
    ],
    optional_entry(
      "execution",
      document.execution,
      blocks.execution_block_to_json,
    ),
  ))
}

/// Decoder for a brief document. Required fields are checked in the schema's
/// property order, so a missing `cluster` is the first failure after `id`.
pub fn brief_document_decoder() -> decode.Decoder(BriefDocument) {
  use id <- decode.field("id", decode.string)
  use cluster <- decode.field("cluster", decode.string)
  use title <- decode.field("title", decode.string)
  use depends_on <- decode.field("depends_on", decode.list(decode.string))
  use blocked_by <- decode.field("blocked_by", decode.list(decode.string))
  use checklist <- decode.field("checklist", decode.list(decode.string))
  use stories <- decode.field("stories", decode.list(decode.string))
  use design_anchor <- decode.field("design_anchor", decode.list(decode.string))
  use purpose <- decode.field("purpose", decode.string)
  use task <- decode.field("task", decode.string)
  use requirements <- decode.field(
    "requirements",
    decode.list(brief_requirement_decoder()),
  )
  use boundaries <- decode.field("boundaries", decode.list(decode.string))
  use verification <- decode.field("verification", decode.list(decode.string))
  use execution <- decode.optional_field(
    "execution",
    option.None,
    decode.map(blocks.execution_block_decoder(), option.Some),
  )
  decode.success(BriefDocument(
    id: id,
    cluster: cluster,
    title: title,
    depends_on: depends_on,
    blocked_by: blocked_by,
    checklist: checklist,
    stories: stories,
    design_anchor: design_anchor,
    purpose: purpose,
    task: task,
    requirements: requirements,
    boundaries: boundaries,
    verification: verification,
    execution: execution,
  ))
}

fn brief_requirement_to_json(requirement: BriefRequirement) -> json.Json {
  json.object(
    list.flatten([
      [
        #("id", json.string(requirement.id)),
        #("title", json.string(requirement.title)),
        #("spec", json.string(requirement.spec)),
        #("acceptance", json.array(requirement.acceptance, json.string)),
        #("files", requirement_files_to_json(requirement.files)),
        #("checklist", json.array(requirement.checklist, json.string)),
        #("stories", json.array(requirement.stories, json.string)),
      ],
      optional_entry("scout", requirement.scout, blocks.scout_block_to_json),
      optional_entry("dev", requirement.dev, blocks.dev_block_to_json),
      optional_entry("review", requirement.review, blocks.review_block_to_json),
    ]),
  )
}

fn brief_requirement_decoder() -> decode.Decoder(BriefRequirement) {
  use id <- decode.field("id", decode.string)
  use title <- decode.field("title", decode.string)
  use spec <- decode.field("spec", decode.string)
  use acceptance <- decode.field("acceptance", decode.list(decode.string))
  use files <- decode.field("files", requirement_files_decoder())
  use checklist <- decode.field("checklist", decode.list(decode.string))
  use stories <- decode.field("stories", decode.list(decode.string))
  use scout <- decode.optional_field(
    "scout",
    option.None,
    decode.map(blocks.scout_block_decoder(), option.Some),
  )
  use dev <- decode.optional_field(
    "dev",
    option.None,
    decode.map(blocks.dev_block_decoder(), option.Some),
  )
  use review <- decode.optional_field(
    "review",
    option.None,
    decode.map(blocks.review_block_decoder(), option.Some),
  )
  decode.success(BriefRequirement(
    id: id,
    title: title,
    spec: spec,
    acceptance: acceptance,
    files: files,
    checklist: checklist,
    stories: stories,
    scout: scout,
    dev: dev,
    review: review,
  ))
}

fn requirement_files_to_json(files: RequirementFiles) -> json.Json {
  json.object([
    #("create", json.array(files.create, json.string)),
    #("modify", json.array(files.modify, json.string)),
    #("delete", json.array(files.delete, json.string)),
  ])
}

fn requirement_files_decoder() -> decode.Decoder(RequirementFiles) {
  use create <- decode.field("create", decode.list(decode.string))
  use modify <- decode.field("modify", decode.list(decode.string))
  use delete <- decode.field("delete", decode.list(decode.string))
  decode.success(RequirementFiles(
    create: create,
    modify: modify,
    delete: delete,
  ))
}

/// Codec for the pre-resolved reference context assembled at dispatch time.
pub fn resolved_context_codec() -> codec.Codec(ResolvedContext) {
  codec.json_codec(resolved_context_to_json, resolved_context_decoder())
}

/// JSON encoder for a resolved context.
pub fn resolved_context_to_json(context: ResolvedContext) -> json.Json {
  json.object([
    #("adrs", json.array(context.adrs, resolved_adr_to_json)),
    #("checklist", json.array(context.checklist, resolved_item_to_json)),
    #("stories", json.array(context.stories, resolved_item_to_json)),
    #("constraints", json.array(context.constraints, resolved_item_to_json)),
    #("intention", json.string(context.intention)),
    #("design_path", json.string(context.design_path)),
    #("provenance", resolved_provenance_to_json(context.provenance)),
  ])
}

/// Decoder for a resolved context.
pub fn resolved_context_decoder() -> decode.Decoder(ResolvedContext) {
  use adrs <- decode.field("adrs", decode.list(resolved_adr_decoder()))
  use checklist <- decode.field(
    "checklist",
    decode.list(resolved_item_decoder()),
  )
  use stories <- decode.field("stories", decode.list(resolved_item_decoder()))
  use constraints <- decode.field(
    "constraints",
    decode.list(resolved_item_decoder()),
  )
  use intention <- decode.field("intention", decode.string)
  use design_path <- decode.field("design_path", decode.string)
  use provenance <- decode.field("provenance", resolved_provenance_decoder())
  decode.success(ResolvedContext(
    adrs: adrs,
    checklist: checklist,
    stories: stories,
    constraints: constraints,
    intention: intention,
    design_path: design_path,
    provenance: provenance,
  ))
}

fn resolved_adr_to_json(adr: ResolvedAdr) -> json.Json {
  json.object([
    #("id", json.string(adr.id)),
    #("title", json.string(adr.title)),
    #("decision", json.string(adr.decision)),
    #("quote", json.string(adr.quote)),
    #("decided_by", json.string(adr.decided_by)),
  ])
}

fn resolved_adr_decoder() -> decode.Decoder(ResolvedAdr) {
  use id <- decode.field("id", decode.string)
  use title <- decode.field("title", decode.string)
  use decision <- decode.field("decision", decode.string)
  use quote <- decode.field("quote", decode.string)
  use decided_by <- decode.field("decided_by", decode.string)
  decode.success(ResolvedAdr(
    id: id,
    title: title,
    decision: decision,
    quote: quote,
    decided_by: decided_by,
  ))
}

fn resolved_item_to_json(item: ResolvedItem) -> json.Json {
  json.object([
    #("id", json.string(item.id)),
    #("text", json.string(item.text)),
  ])
}

fn resolved_item_decoder() -> decode.Decoder(ResolvedItem) {
  use id <- decode.field("id", decode.string)
  use text <- decode.field("text", decode.string)
  decode.success(ResolvedItem(id: id, text: text))
}

fn resolved_provenance_to_json(provenance: ResolvedProvenance) -> json.Json {
  json.object([
    #("requested_by", json.string(provenance.requested_by)),
    #("quote", json.string(provenance.quote)),
  ])
}

fn resolved_provenance_decoder() -> decode.Decoder(ResolvedProvenance) {
  use requested_by <- decode.field("requested_by", decode.string)
  use quote <- decode.field("quote", decode.string)
  decode.success(ResolvedProvenance(requested_by: requested_by, quote: quote))
}

/// One key-value entry when the optional value is present; no entry at all
/// when it is `None` — absent enrichment is absent from the wire.
fn optional_entry(
  key: String,
  value: option.Option(inner),
  encode: fn(inner) -> json.Json,
) -> List(#(String, json.Json)) {
  case value {
    option.Some(inner) -> [#(key, encode(inner))]
    option.None -> []
  }
}
