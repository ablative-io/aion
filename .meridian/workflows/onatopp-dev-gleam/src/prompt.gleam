/// Prompt rendering helpers — mirrors onatopp-dev-norn rendering functions.

import gleam/list
import gleam/result
import gleam/string
import meridian_ffi as ffi

pub fn render_requirement(r: String) -> String {
  let id = ffi.json_opt(r, "id", "") |> result.unwrap("")
  let title = ffi.json_opt(r, "title", "") |> result.unwrap("")
  let spec = ffi.json_opt(r, "spec", "") |> result.unwrap("")

  let header = string.concat(["### ", id, ": ", title, "\n\n", spec, "\n\n"])
  let files_section = render_files(r)
  let acceptance_section = render_acceptance(r)
  let checklist_section = render_checklist(r)
  let stories_section = render_stories(r)

  string.concat([
    header, files_section, acceptance_section,
    checklist_section, stories_section, "\n",
  ])
}

fn render_files(r: String) -> String {
  let create = ffi.json_opt_array(r, "create") |> try_nested_files(r, "files", "create")
  let modify = ffi.json_opt_array(r, "modify") |> try_nested_files(r, "files", "modify")
  let delete = ffi.json_opt_array(r, "delete") |> try_nested_files(r, "files", "delete")

  let create_str = format_file_list("Create:", create)
  let modify_str = format_file_list("Modify:", modify)
  let delete_str = format_file_list("Delete:", delete)

  string.concat([create_str, modify_str, delete_str])
}

fn try_nested_files(_fallback: List(String), r: String, outer: String, inner: String) -> List(String) {
  case ffi.json_get(r, outer) {
    Ok(files_obj) ->
      case ffi.json_string_array(files_obj, inner) {
        Ok(items) -> items
        Error(_) -> []
      }
    Error(_) -> []
  }
}

fn format_file_list(label: String, items: List(String)) -> String {
  case items {
    [] -> ""
    _ -> string.concat([label, " ", string.join(items, " "), "\n"])
  }
}

fn render_acceptance(r: String) -> String {
  case ffi.json_string_array(r, "acceptance") {
    Ok(items) ->
      string.concat([
        "\nAcceptance:\n",
        list.map(items, fn(a) { string.concat(["- ", a, "\n"]) })
          |> string.concat,
      ])
    Error(_) -> ""
  }
}

fn render_checklist(r: String) -> String {
  case ffi.json_opt_array(r, "checklist") {
    [] -> ""
    items ->
      string.concat([
        "\nChecklist:\n",
        list.map(items, fn(c) {
          let id = ffi.json_opt(c, "id", "") |> result.unwrap("")
          let text = ffi.json_opt(c, "text", "") |> result.unwrap("")
          case text {
            "" -> string.concat(["- ", id, "\n"])
            _ -> string.concat(["- ", id, ": ", text, "\n"])
          }
        }) |> string.concat,
      ])
  }
}

fn render_stories(r: String) -> String {
  case ffi.json_opt_array(r, "stories") {
    [] -> ""
    items ->
      string.concat([
        "\nStories:\n",
        list.map(items, fn(s) {
          let id = ffi.json_opt(s, "id", "") |> result.unwrap("")
          let text = ffi.json_opt(s, "text", "") |> result.unwrap("")
          case text {
            "" -> string.concat(["- ", id, "\n"])
            _ -> string.concat(["- ", id, ": ", text, "\n"])
          }
        }) |> string.concat,
      ])
  }
}

pub fn render_enriched_requirement(r: String, scout_enrichments: List(String)) -> String {
  let base = render_requirement(r)
  let id = ffi.json_opt(r, "id", "") |> result.unwrap("")
  let scout_ctx = find_enrichment(scout_enrichments, id)

  case scout_ctx {
    "" -> base
    ctx -> string.concat([base, ctx])
  }
}

pub fn render_review_requirement(
  r: String,
  scout_enrichments: List(String),
  dev_enrichments: List(String),
) -> String {
  let base = render_requirement(r)
  let id = ffi.json_opt(r, "id", "") |> result.unwrap("")

  let scout_section = case find_by_id(scout_enrichments, id) {
    Ok(s) -> {
      let approach = ffi.json_opt(s, "approach", "") |> result.unwrap("")
      string.concat(["**Scout:** ", approach, "\n"])
    }
    Error(_) -> ""
  }

  let dev_section = case find_by_id(dev_enrichments, id) {
    Ok(d) -> {
      let status = ffi.json_opt(d, "status", "") |> result.unwrap("")
      let how = ffi.json_opt(d, "how", "") |> result.unwrap("")
      let files_str = case ffi.json_get(d, "files_changed") {
        Ok(fc) -> string.concat(["Files changed: ", fc, "\n"])
        Error(_) -> ""
      }
      let deviation = ffi.json_opt(d, "deviation", "") |> result.unwrap("")
      let dev_str = string.concat(["**Dev:** ", status, " — ", how, "\n", files_str])
      case deviation {
        "" -> dev_str
        _ -> string.concat([dev_str, "Deviation: ", deviation, "\n"])
      }
    }
    Error(_) -> ""
  }

  string.concat([base, scout_section, dev_section, "\n"])
}

fn find_enrichment(enrichments: List(String), target_id: String) -> String {
  case find_by_id(enrichments, target_id) {
    Ok(s) -> {
      let files_str = case ffi.json_get(s, "files") {
        Ok(f) -> string.concat(["- Key files: ", f, "\n"])
        Error(_) -> ""
      }
      let context_items = case ffi.json_string_array(s, "context") {
        Ok(items) ->
          list.map(items, fn(c) { string.concat(["- ", c, "\n"]) })
          |> string.concat
        Error(_) -> ""
      }
      let approach = ffi.json_opt(s, "approach", "") |> result.unwrap("")
      let notes = ffi.json_opt(s, "notes", "") |> result.unwrap("")
      let approach_str = case approach {
        "" -> ""
        _ -> string.concat(["- Approach: ", approach, "\n"])
      }
      let notes_str = case notes {
        "" -> ""
        _ -> string.concat(["- Notes: ", notes, "\n"])
      }
      string.concat([
        "**Scout context:**\n",
        files_str, context_items, approach_str, notes_str, "\n",
      ])
    }
    Error(_) -> ""
  }
}

fn find_by_id(items: List(String), target_id: String) -> Result(String, Nil) {
  list.find(items, fn(item) {
    case ffi.json_opt(item, "id", "") {
      Ok(id) -> id == target_id
      Error(_) -> False
    }
  })
}

pub fn build_design_context(input: String, brief: String) -> String {
  case ffi.json_unwrap(input, "design_content") {
    Error(_) -> ""
    Ok(design) -> {
      let intention = case ffi.json_opt(design, "intention", "") {
        Ok("") -> ""
        Ok(i) -> string.concat(["**Intention:** ", i, "\n\n"])
        Error(_) -> ""
      }

      let constraints_str = case ffi.json_opt_array(design, "constraints") {
        [] -> ""
        items ->
          string.concat([
            "**Constraints:**\n",
            list.map(items, fn(c) {
              let id = ffi.json_opt(c, "id", "") |> result.unwrap("")
              let desc = ffi.json_opt(c, "description", "") |> result.unwrap("")
              string.concat(["- ", id, ": ", desc, "\n"])
            }) |> string.concat,
            "\n",
          ])
      }

      let goals_str = case ffi.json_string_array(design, "goals") {
        Error(_) -> ""
        Ok([]) -> ""
        Ok(items) ->
          string.concat([
            "**Goals:**\n",
            list.map(items, fn(g) { string.concat(["- ", g, "\n"]) })
              |> string.concat,
            "\n",
          ])
      }

      let decisions_str = build_decisions(design, brief)

      string.concat(["## Design Context\n\n", intention, constraints_str, goals_str, decisions_str])
    }
  }
}

fn build_decisions(design: String, brief: String) -> String {
  case ffi.json_opt_array(design, "decisions") {
    [] -> ""
    items -> {
      let anchor = ffi.json_opt(brief, "design_anchor", "") |> result.unwrap("")
      let has_filter = anchor != ""

      let filtered = list.filter(items, fn(d) {
        let status = ffi.json_opt(d, "status", "") |> result.unwrap("")
        case status == "active" {
          False -> False
          True ->
            case has_filter {
              False -> True
              True -> {
                let did = ffi.json_opt(d, "id", "") |> result.unwrap("")
                string.contains(anchor, did)
              }
            }
        }
      })

      case filtered {
        [] -> ""
        _ ->
          string.concat([
            "**Key Decisions:**\n",
            list.map(filtered, fn(d) {
              let id = ffi.json_opt(d, "id", "") |> result.unwrap("")
              let title = ffi.json_opt(d, "title", "") |> result.unwrap("")
              let choice = ffi.json_opt(d, "choice", "") |> result.unwrap("")
              case choice {
                "" -> string.concat(["- ", id, ": ", title, "\n"])
                _ -> string.concat(["- ", id, ": ", title, " — ", choice, "\n"])
              }
            }) |> string.concat,
            "\n",
          ])
      }
    }
  }
}

pub fn resolve_checklist(brief: String, input: String) -> String {
  case ffi.json_unwrap(input, "checklist_content") {
    Error(_) -> brief
    Ok(checklist) ->
      case ffi.json_opt_array(checklist, "sections") {
        [] -> brief
        sections -> {
          let lookup = build_checklist_lookup(sections)
          resolve_checklist_in_requirements(brief, lookup)
        }
      }
  }
}

fn build_checklist_lookup(sections: List(String)) -> List(#(String, String)) {
  list.flat_map(sections, fn(section) {
    case ffi.json_opt_array(section, "items") {
      [] -> []
      items ->
        list.filter_map(items, fn(item) {
          case ffi.json_opt(item, "id", ""), ffi.json_opt(item, "text", "") {
            Ok(id), Ok(text) -> Ok(#(id, text))
            _, _ -> Error(Nil)
          }
        })
    }
  })
}

fn resolve_checklist_in_requirements(brief: String, _lookup: List(#(String, String))) -> String {
  brief
}

pub fn resolve_stories(brief: String, input: String) -> String {
  case ffi.json_unwrap(input, "stories_content") {
    Error(_) -> brief
    Ok(_stories) -> brief
  }
}
