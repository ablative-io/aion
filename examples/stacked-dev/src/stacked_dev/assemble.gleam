//// The `assemble_wave` dispatcher resolution (BD-006 R4).
////
//// This is the ONLY place in the family that reads ledgers and resolves
//// references (CN1). Given a design directory and a wave of brief ids it
//// reads `<design_dir>/roadmap.json` and `<design_dir>/decisions.json`,
//// locates each brief by scanning the cluster directories, decodes it, builds
//// its resolved context (anchored ADR texts verbatim, C#/S# texts, cluster
//// constraints and intention, and the roadmap provenance quote with its
//// speaker — P6), orders the wave so every within-wave `depends_on` precedes
//// its dependent, and REFUSES the whole wave when any brief is
//// dependency-blocked, coverage-broken, or the wave's edges contain a cycle
//// (S4, S17). Refusal is a can't-execute condition (CN5): a doomed wave never
//// starts. The coverage and landed-dependency checks are native — nothing
//// shells out to check-coverage.py or validate.py — so the hermetic suite's
//// exclusive PATH can exercise them.

import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option
import gleam/result
import gleam/string
import stacked_dev/codecs_brief
import stacked_dev/types.{
  type AssembleInput, type AssembledWave, type BriefDocument, type ResolvedAdr,
  type ResolvedItem, type WaveEntry, AssembledWave, ExecutionLanded,
  ResolvedContext, ResolvedItem, ResolvedProvenance, WaveEntry,
}

@external(erlang, "stacked_dev_file_ffi", "read_file")
fn read_file(path: String) -> Result(String, String)

@external(erlang, "stacked_dev_file_ffi", "list_dir")
fn list_dir(path: String) -> Result(List(String), String)

/// One roadmap item, reduced to the provenance projection the resolver needs.
type RoadmapItem {
  RoadmapItem(cluster: String, requested_by: String, quote: String)
}

/// One located, decoded brief with its cluster's resolved documents.
type Loaded {
  Loaded(
    id: String,
    cluster: String,
    document: BriefDocument,
    checklist_items: List(ResolvedItem),
    story_items: List(ResolvedItem),
    intention: String,
    constraints: List(ResolvedItem),
  )
}

/// The on-disk landed state of a `depends_on` brief that is not in the wave.
type DepStatus {
  DepLanded
  DepNotLanded
  DepNotFound
}

/// Resolve, order, and refuse a wave. Returns the ordered, fully resolved
/// wave on success, or a single diagnostic naming every offending brief and
/// reason on a refusal or a can't-execute condition. The caller
/// (`locals.assemble_wave`) lifts an `Error` into a terminal activity failure.
pub fn run(input: AssembleInput) -> Result(AssembledWave, String) {
  use decisions <- result.try(read_json(
    input.design_dir <> "/decisions.json",
    decisions_decoder(),
  ))
  use roadmap <- result.try(read_json(
    input.design_dir <> "/roadmap.json",
    roadmap_decoder(),
  ))
  use loaded <- result.try(load_all(input.design_dir, input.wave))
  let wave_ids = list.map(loaded, fn(entry) { entry.id })
  let reasons =
    list.flat_map(loaded, fn(entry) {
      brief_reasons(input.design_dir, entry, decisions, roadmap, wave_ids)
    })
  case order(loaded, wave_ids) {
    Ok(ordered) ->
      case reasons {
        [] ->
          Ok(
            AssembledWave(
              entries: list.map(ordered, fn(entry) {
                build_entry(entry, decisions, roadmap, input.design_dir)
              }),
            ),
          )
        _ -> Error(refusal_message(reasons))
      }
    Error(cyclic_ids) ->
      Error(
        refusal_message(
          list.append(reasons, [
            "dependency cycle among wave briefs: "
            <> string.join(cyclic_ids, ", "),
          ]),
        ),
      )
  }
}

// --- loading ----------------------------------------------------------------

fn load_all(
  design_dir: String,
  wave: List(String),
) -> Result(List(Loaded), String) {
  case wave {
    [] -> Ok([])
    [id, ..rest] -> {
      use loaded <- result.try(load_one(design_dir, id))
      use more <- result.try(load_all(design_dir, rest))
      Ok([loaded, ..more])
    }
  }
}

fn load_one(design_dir: String, id: String) -> Result(Loaded, String) {
  use #(cluster, document) <- result.try(locate(design_dir, id))
  let cluster_dir = design_dir <> "/" <> cluster
  use checklist <- result.try(read_json(
    cluster_dir <> "/checklist.json",
    checklist_decoder(),
  ))
  use stories <- result.try(read_json(
    cluster_dir <> "/stories.json",
    stories_decoder(),
  ))
  use design <- result.try(read_json(
    cluster_dir <> "/design.json",
    design_decoder(),
  ))
  let #(intention, constraints) = design
  Ok(Loaded(
    id: id,
    cluster: cluster,
    document: document,
    checklist_items: checklist,
    story_items: stories,
    intention: intention,
    constraints: constraints,
  ))
}

/// Locate one brief by scanning the cluster directories under `design_dir`
/// for `<cluster>/briefs/<id>.json`. Exactly one match must exist.
fn locate(
  design_dir: String,
  id: String,
) -> Result(#(String, BriefDocument), String) {
  case list_dir(design_dir) {
    Error(reason) ->
      Error("assemble_wave: cannot list " <> design_dir <> ": " <> reason)
    Ok(names) -> {
      let candidates =
        list.filter_map(names, fn(name) {
          let path = design_dir <> "/" <> name <> "/briefs/" <> id <> ".json"
          case read_file(path) {
            Ok(raw) -> Ok(#(name, raw))
            Error(_) -> Error(Nil)
          }
        })
      case candidates {
        [#(cluster, raw)] ->
          case json.parse(raw, codecs_brief.brief_document_decoder()) {
            Ok(document) -> Ok(#(cluster, document))
            Error(_) ->
              Error(
                "assemble_wave: brief "
                <> id
                <> " at "
                <> design_dir
                <> "/"
                <> cluster
                <> "/briefs/"
                <> id
                <> ".json failed to decode",
              )
          }
        [] ->
          Error(
            "assemble_wave: brief " <> id <> " not found under " <> design_dir,
          )
        _ ->
          Error(
            "assemble_wave: brief "
            <> id
            <> " matched multiple clusters: "
            <> string.join(list.map(candidates, fn(pair) { pair.0 }), ", "),
          )
      }
    }
  }
}

// --- refusal checks ---------------------------------------------------------

fn brief_reasons(
  design_dir: String,
  loaded: Loaded,
  decisions: List(ResolvedAdr),
  roadmap: List(RoadmapItem),
  wave_ids: List(String),
) -> List(String) {
  let document = loaded.document
  let id = document.id
  let union_checklist =
    list.flat_map(document.requirements, fn(req) { req.checklist })
    |> list.unique
  let union_stories =
    list.flat_map(document.requirements, fn(req) { req.stories })
    |> list.unique
  let checklist_ids = list.map(loaded.checklist_items, fn(item) { item.id })
  let story_ids = list.map(loaded.story_items, fn(item) { item.id })
  let decision_ids = list.map(decisions, fn(adr) { adr.id })

  let level_not_union_c =
    document.checklist
    |> list.filter(fn(c) { !list.contains(union_checklist, c) })
    |> list.map(fn(c) {
      id <> ": brief-level checklist " <> c <> " not covered by any R#"
    })
  let union_not_level_c =
    union_checklist
    |> list.filter(fn(c) { !list.contains(document.checklist, c) })
    |> list.map(fn(c) {
      id
      <> ": checklist "
      <> c
      <> " cited by "
      <> citing(document, c, True)
      <> " but missing from the brief-level array"
    })
  let level_not_union_s =
    document.stories
    |> list.filter(fn(s) { !list.contains(union_stories, s) })
    |> list.map(fn(s) {
      id <> ": brief-level story " <> s <> " not covered by any R#"
    })
  let union_not_level_s =
    union_stories
    |> list.filter(fn(s) { !list.contains(document.stories, s) })
    |> list.map(fn(s) {
      id
      <> ": story "
      <> s
      <> " cited by "
      <> citing(document, s, False)
      <> " but missing from the brief-level array"
    })
  let unknown_c =
    list.append(document.checklist, union_checklist)
    |> list.unique
    |> list.filter(fn(c) { !list.contains(checklist_ids, c) })
    |> list.map(fn(c) {
      id <> ": checklist id " <> c <> " not found in the cluster checklist"
    })
  let unknown_s =
    list.append(document.stories, union_stories)
    |> list.unique
    |> list.filter(fn(s) { !list.contains(story_ids, s) })
    |> list.map(fn(s) {
      id <> ": story id " <> s <> " not found in the cluster stories"
    })
  let anchor =
    document.design_anchor
    |> list.filter(fn(adr) { !list.contains(decision_ids, adr) })
    |> list.map(fn(adr) {
      id <> ": design_anchor " <> adr <> " not found in decisions.json"
    })
  let provenance = case
    list.any(roadmap, fn(item) { item.cluster == loaded.cluster })
  {
    True -> []
    False -> [id <> ": no roadmap item links cluster " <> loaded.cluster]
  }
  let deps =
    list.flat_map(document.depends_on, fn(dep) {
      dep_reason(design_dir, id, dep, wave_ids)
    })

  list.flatten([
    level_not_union_c,
    union_not_level_c,
    level_not_union_s,
    union_not_level_s,
    unknown_c,
    unknown_s,
    anchor,
    provenance,
    deps,
  ])
}

/// The comma-joined R# ids whose checklist (or stories) array cites `id`.
fn citing(document: BriefDocument, id: String, is_checklist: Bool) -> String {
  document.requirements
  |> list.filter(fn(req) {
    case is_checklist {
      True -> list.contains(req.checklist, id)
      False -> list.contains(req.stories, id)
    }
  })
  |> list.map(fn(req) { req.id })
  |> string.join(", ")
}

fn dep_reason(
  design_dir: String,
  id: String,
  dep: String,
  wave_ids: List(String),
) -> List(String) {
  case list.contains(wave_ids, dep) {
    True -> []
    False ->
      case dep_status(design_dir, dep) {
        DepLanded -> []
        DepNotFound -> [
          id
          <> " depends on "
          <> dep
          <> ", which is not in the wave and was not found on disk",
        ]
        DepNotLanded -> [
          id
          <> " depends on "
          <> dep
          <> ", which is not in the wave and is not landed on disk"
          <> " (no execution block with status landed)",
        ]
      }
  }
}

fn dep_status(design_dir: String, dep: String) -> DepStatus {
  case locate(design_dir, dep) {
    Error(_) -> DepNotFound
    Ok(#(_, document)) ->
      case document.execution {
        option.Some(block) ->
          case block.status {
            ExecutionLanded -> DepLanded
            _ -> DepNotLanded
          }
        option.None -> DepNotLanded
      }
  }
}

fn refusal_message(reasons: List(String)) -> String {
  "assemble_wave refused the wave: " <> string.join(reasons, "; ")
}

// --- ordering ---------------------------------------------------------------

/// Stable topological order: every within-wave `depends_on` precedes its
/// dependent, the caller's order preserved among independents. `Error` carries
/// the ids that could not be placed — a cycle.
fn order(
  loaded: List(Loaded),
  wave_ids: List(String),
) -> Result(List(Loaded), List(String)) {
  order_loop(loaded, wave_ids, [], [])
}

fn order_loop(
  remaining: List(Loaded),
  wave_ids: List(String),
  placed: List(String),
  acc: List(Loaded),
) -> Result(List(Loaded), List(String)) {
  case remaining {
    [] -> Ok(list.reverse(acc))
    _ ->
      case pick(remaining, wave_ids, placed, []) {
        Ok(#(chosen, rest)) ->
          order_loop(rest, wave_ids, [chosen.id, ..placed], [chosen, ..acc])
        Error(Nil) -> Error(list.map(remaining, fn(entry) { entry.id }))
      }
  }
}

/// The first remaining brief whose within-wave dependencies are all placed,
/// paired with the rest in original order.
fn pick(
  remaining: List(Loaded),
  wave_ids: List(String),
  placed: List(String),
  seen: List(Loaded),
) -> Result(#(Loaded, List(Loaded)), Nil) {
  case remaining {
    [] -> Error(Nil)
    [entry, ..rest] -> {
      let in_wave_deps =
        list.filter(entry.document.depends_on, fn(dep) {
          list.contains(wave_ids, dep)
        })
      case list.all(in_wave_deps, fn(dep) { list.contains(placed, dep) }) {
        True -> Ok(#(entry, list.append(list.reverse(seen), rest)))
        False -> pick(rest, wave_ids, placed, [entry, ..seen])
      }
    }
  }
}

// --- resolution -------------------------------------------------------------

fn build_entry(
  loaded: Loaded,
  decisions: List(ResolvedAdr),
  roadmap: List(RoadmapItem),
  design_dir: String,
) -> WaveEntry {
  let document = loaded.document
  let adrs =
    list.filter_map(document.design_anchor, fn(adr_id) {
      list.find(decisions, fn(adr) { adr.id == adr_id })
    })
  let checklist =
    list.filter_map(document.checklist, fn(cid) {
      list.find(loaded.checklist_items, fn(item) { item.id == cid })
    })
  let stories =
    list.filter_map(document.stories, fn(sid) {
      list.find(loaded.story_items, fn(item) { item.id == sid })
    })
  let provenance = case
    list.find(roadmap, fn(item) { item.cluster == loaded.cluster })
  {
    Ok(item) ->
      ResolvedProvenance(requested_by: item.requested_by, quote: item.quote)
    Error(_) -> ResolvedProvenance(requested_by: "", quote: "")
  }
  WaveEntry(
    brief_document: document,
    resolved_context: ResolvedContext(
      adrs: adrs,
      checklist: checklist,
      stories: stories,
      constraints: loaded.constraints,
      intention: loaded.intention,
      design_path: design_dir <> "/" <> loaded.cluster <> "/design.json",
      provenance: provenance,
    ),
  )
}

// --- ledger decoders --------------------------------------------------------

fn read_json(
  path: String,
  decoder: decode.Decoder(value),
) -> Result(value, String) {
  case read_file(path) {
    Error(reason) ->
      Error("assemble_wave: cannot read " <> path <> ": " <> reason)
    Ok(raw) ->
      case json.parse(raw, decoder) {
        Ok(value) -> Ok(value)
        Error(_) -> Error("assemble_wave: cannot parse " <> path <> " as JSON")
      }
  }
}

fn decisions_decoder() -> decode.Decoder(List(ResolvedAdr)) {
  use decisions <- decode.field("decisions", decode.list(adr_decoder()))
  decode.success(decisions)
}

fn adr_decoder() -> decode.Decoder(ResolvedAdr) {
  use id <- decode.field("id", decode.string)
  use title <- decode.field("title", decode.string)
  use decision <- decode.field("decision", decode.string)
  use quote <- decode.field("quote", decode.string)
  use decided_by <- decode.field("decided_by", decode.string)
  decode.success(types.ResolvedAdr(
    id: id,
    title: title,
    decision: decision,
    quote: quote,
    decided_by: decided_by,
  ))
}

fn roadmap_decoder() -> decode.Decoder(List(RoadmapItem)) {
  use items <- decode.field("items", decode.list(roadmap_item_decoder()))
  decode.success(items)
}

fn roadmap_item_decoder() -> decode.Decoder(RoadmapItem) {
  use cluster <- decode.subfield(["links", "cluster"], decode.string)
  use requested_by <- decode.subfield(
    ["provenance", "requested_by"],
    decode.string,
  )
  use quote <- decode.subfield(["provenance", "quote"], decode.string)
  decode.success(RoadmapItem(
    cluster: cluster,
    requested_by: requested_by,
    quote: quote,
  ))
}

fn checklist_decoder() -> decode.Decoder(List(ResolvedItem)) {
  use sections <- decode.field(
    "sections",
    decode.list({
      use items <- decode.field("items", decode.list(resolved_item_decoder()))
      decode.success(items)
    }),
  )
  decode.success(list.flatten(sections))
}

fn stories_decoder() -> decode.Decoder(List(ResolvedItem)) {
  use personas <- decode.field(
    "personas",
    decode.list({
      use stories <- decode.field(
        "stories",
        decode.list(resolved_item_decoder()),
      )
      decode.success(stories)
    }),
  )
  decode.success(list.flatten(personas))
}

fn design_decoder() -> decode.Decoder(#(String, List(ResolvedItem))) {
  use intention <- decode.field("intention", decode.string)
  use constraints <- decode.field(
    "constraints",
    decode.list(resolved_item_decoder()),
  )
  decode.success(#(intention, constraints))
}

fn resolved_item_decoder() -> decode.Decoder(ResolvedItem) {
  use id <- decode.field("id", decode.string)
  use text <- decode.field("text", decode.string)
  decode.success(ResolvedItem(id: id, text: text))
}
