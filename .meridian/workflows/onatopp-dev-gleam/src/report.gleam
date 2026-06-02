/// Completion report builder — mirrors onatopp-dev-norn build_report function.

import gleam/list
import gleam/result
import gleam/string
import meridian_ffi as ffi

pub fn build_report(
  brief: String,
  dev: String,
  dev_enrichments: List(String),
  review: String,
  review_enrichments: List(String),
) -> String {
  let brief_id = ffi.json_opt(brief, "id", "") |> result.unwrap("")
  let brief_title = ffi.json_opt(brief, "title", "") |> result.unwrap("")
  let pass = ffi.json_bool(review, "pass") |> result.unwrap(False)
  let status = case pass { True -> "PASSED" False -> "FAILED" }

  let dev_summary = ffi.json_opt(dev, "summary", "") |> result.unwrap("")
  let review_summary = ffi.json_opt(review, "summary", "") |> result.unwrap("")

  let header = string.concat([
    "onatopp-dev-gleam: ", brief_id, " — ", brief_title, "\n",
    "Status: ", status, "\n\n",
    "Dev: ", dev_summary, "\n",
    "Review: ", review_summary, "\n\n",
  ])

  let requirements = ffi.json_opt_array(brief, "requirements")
  let req_details = list.map(requirements, fn(r) {
    let id = ffi.json_opt(r, "id", "") |> result.unwrap("")
    let title = ffi.json_opt(r, "title", "") |> result.unwrap("")
    let req_header = string.concat([id, ": ", title, "\n"])

    let detail = case find_by_id(dev_enrichments, id), find_by_id(review_enrichments, id) {
      Ok(d), Ok(rv) -> {
        let d_status = ffi.json_opt(d, "status", "") |> result.unwrap("")
        let rv_alignment = ffi.json_opt(rv, "alignment", "") |> result.unwrap("")
        let acc_met = ffi.json_bool(rv, "acceptance_met") |> result.unwrap(False)
        let acc_str = case acc_met { True -> "met" False -> "not met" }

        let issues = ffi.json_string_array(rv, "issues") |> result.unwrap([])
        let fixes = ffi.json_string_array(rv, "fixes") |> result.unwrap([])

        let issues_str = list.map(issues, fn(i) { string.concat(["  Issue: ", i, "\n"]) })
          |> string.concat
        let fixes_str = list.map(fixes, fn(f) { string.concat(["  Fixed: ", f, "\n"]) })
          |> string.concat

        string.concat([
          "  ", d_status, " | ", rv_alignment, " | acceptance ", acc_str, "\n",
          issues_str, fixes_str,
        ])
      }
      Ok(d), Error(_) -> {
        let d_status = ffi.json_opt(d, "status", "") |> result.unwrap("")
        let how = ffi.json_opt(d, "how", "") |> result.unwrap("")
        string.concat(["  ", d_status, " — ", how, "\n"])
      }
      _, _ -> ""
    }
    string.concat([req_header, detail, "\n"])
  }) |> string.concat

  string.concat([header, req_details])
}

fn find_by_id(items: List(String), target_id: String) -> Result(String, Nil) {
  list.find(items, fn(item) {
    case ffi.json_opt(item, "id", "") {
      Ok(id) -> id == target_id
      Error(_) -> False
    }
  })
}
