/// onatopp-dev-gleam — All-Norn dev cycle in Gleam.
///
/// Pipeline: Scout → Dev → Commit → Review+Harden (max 2) → Commit → Notify
///
/// Full parity with onatopp-dev-norn: C#/S# resolution, design context,
/// progressive enrichment, structured schemas, failure notification, reports.

import gleam/int
import gleam/io
import gleam/list
import gleam/result
import gleam/string
import meridian_ffi as ffi
import prompt
import report
import schema

pub fn run(input_json: String) -> String {
  case do_workflow(input_json) {
    Ok(report_text) -> report_text
    Error(reason) -> {
      maybe_notify_failure(input_json, reason)
      string.concat(["FAILED: ", reason])
    }
  }
}

fn do_workflow(input_json: String) -> Result(String, String) {
  use _ <- result.try(ffi.set_context("input", input_json))

  use brief_json <- result.try(ffi.json_unwrap(input_json, "brief"))
  use brief_id <- result.try(ffi.json_string(brief_json, "id"))
  use brief_title <- result.try(ffi.json_string(brief_json, "title"))
  let brief_purpose = ffi.json_opt(brief_json, "purpose", "") |> result.unwrap("")
  let brief_task = ffi.json_opt(brief_json, "task", "") |> result.unwrap("")
  let cluster = ffi.json_opt(brief_json, "cluster", "") |> result.unwrap("")

  let design_context = prompt.build_design_context(input_json, brief_json)
  let design_file_ref = case cluster {
    "" -> ""
    c -> string.concat(["Full design document: docs/design/", c, "/DESIGN.md\n"])
  }

  let requirements = ffi.json_opt_array(brief_json, "requirements")
  let boundaries = case ffi.json_string_array(brief_json, "boundaries") {
    Ok(b) -> b
    Error(_) -> []
  }

  // Pre-warm build cache
  let _ = ffi.run_cmd("cargo check --workspace --all-targets >/dev/null 2>&1 &")

  // === SCOUT ===

  let scout_instruction = build_scout_instruction(
    design_context, brief_id, brief_title, brief_purpose,
    requirements, boundaries, design_file_ref,
  )

  use scout_output <- result.try(
    do_norn_step("scout", "norn-codebase-explorer", scout_instruction, schema.scout_schema())
  )
  use _ <- result.try(ffi.set_context("scout_output", scout_output))
  io.println(string.concat(["Scout complete: ", ffi.json_opt(scout_output, "summary", "") |> result.unwrap("")]))

  let scout_enrichments = ffi.json_opt_array(scout_output, "enrichments")
  let scout_verification = case ffi.json_string_array(scout_output, "verification") {
    Ok(v) -> v
    Error(_) -> []
  }

  // === DEV ===

  let dev_instruction = build_dev_instruction(
    design_context, brief_id, brief_title, brief_task,
    requirements, scout_enrichments, boundaries,
    scout_verification, design_file_ref,
  )

  use dev_output <- result.try(
    do_norn_step("dev", "norn-developer", dev_instruction, schema.dev_schema())
  )
  use _ <- result.try(ffi.set_context("dev_output", dev_output))
  io.println(string.concat(["Dev complete: ", ffi.json_opt(dev_output, "summary", "") |> result.unwrap("")]))

  let dev_enrichments = ffi.json_opt_array(dev_output, "enrichments")

  // === COMMIT (after dev) ===

  let dev_commit_msg = ffi.json_opt(dev_output, "commit_message", "feat: dev step complete") |> result.unwrap("feat: dev step complete")
  use _ <- result.try(do_commit(dev_commit_msg))

  // === REVIEW + HARDEN (max 2 attempts) ===

  use #(review_output, review_passed) <- result.try(
    do_review_loop(
      1, 2, design_context, brief_id, brief_title, brief_json,
      requirements, scout_enrichments, dev_enrichments, dev_output,
      boundaries, scout_verification, design_file_ref,
    )
  )

  let review_enrichments = ffi.json_opt_array(review_output, "enrichments")

  // === COMMIT (after review) ===

  case review_passed {
    True -> {
      let review_commit_msg = ffi.json_opt(review_output, "commit_message", "fix: review fixes") |> result.unwrap("fix: review fixes")
      let _ = do_commit(review_commit_msg)
      Nil
    }
    False -> Nil
  }

  // === DONE + NOTIFY ===

  let status = case review_passed { True -> "PASSED" False -> "FAILED" }
  io.println(string.concat(["Complete: ", brief_id, " — ", status]))

  maybe_notify_success(input_json, brief_id, brief_json, dev_output, dev_enrichments, review_output, review_enrichments)

  Ok(string.concat(["Complete: ", brief_id, " — ", status]))
}

fn do_norn_step(name: String, profile: String, instruction: String, schema_json: String) -> Result(String, String) {
  use output <- result.try(ffi.run_step_norn(name, profile, instruction, schema_json))
  case output {
    "" -> Error(string.concat([name, " returned empty output"]))
    _ -> Ok(output)
  }
}

fn do_commit(message: String) -> Result(String, String) {
  use status <- result.try(ffi.run_cmd("git status --porcelain"))
  case string.length(status) > 0 {
    True -> {
      use _ <- result.try(ffi.write_file(".commit-msg.tmp", message))
      use result_str <- result.try(ffi.run_cmd("bash .meridian/workflows/onatopp-dev-norn/scripts/commit-and-report.sh .commit-msg.tmp"))
      let _ = ffi.run_cmd("rm -f .commit-msg.tmp")
      io.println(string.concat(["Committed: ", message]))
      Ok(result_str)
    }
    False -> Ok("nothing to commit")
  }
}

fn do_review_loop(
  attempt: Int,
  max_attempts: Int,
  design_context: String,
  brief_id: String,
  brief_title: String,
  brief_json: String,
  requirements: List(String),
  scout_enrichments: List(String),
  dev_enrichments: List(String),
  dev_output: String,
  boundaries: List(String),
  scout_verification: List(String),
  design_file_ref: String,
) -> Result(#(String, Bool), String) {
  let review_instruction = build_review_instruction(
    attempt, design_context, brief_id, brief_title, brief_json,
    requirements, scout_enrichments, dev_enrichments, dev_output,
    boundaries, scout_verification, design_file_ref,
  )

  use review_output <- result.try(
    do_norn_step(
      string.concat(["review-", int.to_string(attempt)]),
      "norn-reviewer",
      review_instruction,
      schema.review_schema(),
    )
  )
  let pass = ffi.json_bool(review_output, "pass") |> result.unwrap(False)
  io.println(string.concat(["Review ", int.to_string(attempt), ": pass=", bool_to_string(pass)]))

  case pass {
    True -> Ok(#(review_output, True))
    False ->
      case attempt >= max_attempts {
        True -> {
          io.println(string.concat(["Review failed after ", int.to_string(max_attempts), " attempts"]))
          Ok(#(review_output, False))
        }
        False -> {
          let _ = do_commit(string.concat(["fix: address review findings for ", brief_id]))
          do_review_loop(
            attempt + 1, max_attempts, design_context, brief_id, brief_title,
            brief_json, requirements, scout_enrichments, dev_enrichments,
            dev_output, boundaries, scout_verification, design_file_ref,
          )
        }
      }
  }
}

fn bool_to_string(b: Bool) -> String {
  case b { True -> "true" False -> "false" }
}

// === INSTRUCTION BUILDERS ===

fn build_scout_instruction(
  design_context: String,
  brief_id: String,
  brief_title: String,
  brief_purpose: String,
  requirements: List(String),
  boundaries: List(String),
  design_file_ref: String,
) -> String {
  let req_text = list.map(requirements, prompt.render_requirement) |> string.concat
  let boundaries_text = render_boundaries(boundaries)

  string.concat([
    "Explore the codebase and gather implementation context for each R# in this brief. You are read-only — do not modify files.\n\n",
    "For each R#, find:\n",
    "- 2-5 key files the implementer should look at (with line ranges)\n",
    "- Conventions to match (sibling patterns, naming, error handling)\n",
    "- A concrete implementation approach\n",
    "- Any gotchas or edge cases the brief might not have considered\n\n",
    "The implementing agent has the same tools you do — focus on saving them time, not cataloguing every file. Be concise.\n\n",
    design_context,
    "## Brief: ", brief_id, " — ", brief_title, "\n\n",
    brief_purpose, "\n\n",
    "## Requirements\n\n",
    req_text,
    boundaries_text,
    design_file_ref,
  ])
}

fn build_dev_instruction(
  design_context: String,
  brief_id: String,
  brief_title: String,
  brief_task: String,
  requirements: List(String),
  scout_enrichments: List(String),
  boundaries: List(String),
  scout_verification: List(String),
  design_file_ref: String,
) -> String {
  let req_text = list.map(requirements, fn(r) {
    prompt.render_enriched_requirement(r, scout_enrichments)
  }) |> string.concat
  let boundaries_text = render_boundaries(boundaries)
  let verification_text = render_verification(scout_verification)

  string.concat([
    "Implement every R# in this brief. Run cargo check, cargo clippy -- -D warnings, and cargo test on affected crates. Fix any failures before submitting.\n\n",
    design_context,
    "## Brief: ", brief_id, " — ", brief_title, "\n\n",
    brief_task, "\n\n",
    "## Requirements\n\n",
    req_text,
    boundaries_text,
    verification_text,
    design_file_ref,
    "\nFor each R#, report: status, files changed, how satisfied, any deviation. For each C# and S# assigned to the R#, report whether delivered. Attest: no panics/unwraps in library code, no unsafe, boundaries respected, tests pass.",
  ])
}

fn build_review_instruction(
  _attempt: Int,
  design_context: String,
  brief_id: String,
  brief_title: String,
  brief_json: String,
  requirements: List(String),
  scout_enrichments: List(String),
  dev_enrichments: List(String),
  dev_output: String,
  boundaries: List(String),
  scout_verification: List(String),
  design_file_ref: String,
) -> String {
  let req_text = list.map(requirements, fn(r) {
    prompt.render_review_requirement(r, scout_enrichments, dev_enrichments)
  }) |> string.concat
  let boundaries_text = render_boundaries(boundaries)

  let brief_verification = case ffi.json_string_array(brief_json, "verification") {
    Ok(v) -> v
    Error(_) -> []
  }
  let all_verification = list.append(brief_verification, scout_verification)
  let verification_text = render_verification(all_verification)

  let att = ffi.json_get(dev_output, "attestation") |> result.unwrap("{}")
  let no_panics = ffi.json_bool(att, "no_panics") |> result.unwrap(False)
  let no_unsafe = ffi.json_bool(att, "no_unsafe") |> result.unwrap(False)
  let boundaries_ok = ffi.json_bool(att, "boundaries_respected") |> result.unwrap(False)
  let tests_pass = ffi.json_bool(att, "tests_pass") |> result.unwrap(False)

  string.concat([
    "Review and harden the implementation. You have two jobs:\n\n",
    "1. HARDEN: Fix naming drift, missing error handling, convention violations, edge cases. Use Edit and Write directly.\n",
    "2. REVIEW: Verify acceptance criteria for each R#. Check the ACTUAL CODE (use git diff HEAD~1), not the dev summary. Tick checklist items. Confirm stories.\n\n",
    design_context,
    "## Brief: ", brief_id, " — ", brief_title, "\n\n",
    "## Requirements\n\n",
    req_text,
    boundaries_text,
    "## Verification Criteria\n\n",
    verification_text,
    "Dev attestation: panics=", bool_to_string(no_panics),
    ", unsafe=", bool_to_string(no_unsafe),
    ", boundaries=", bool_to_string(boundaries_ok),
    ", tests=", bool_to_string(tests_pass), "\n\n",
    design_file_ref,
    "\nSet pass=true only if all acceptance criteria are met and no blocking issues remain.",
  ])
}

fn render_boundaries(boundaries: List(String)) -> String {
  case boundaries {
    [] -> ""
    _ ->
      string.concat([
        "## Boundaries\n\n",
        list.map(boundaries, fn(b) { string.concat(["- ", b, "\n"]) }) |> string.concat,
        "\n",
      ])
  }
}

fn render_verification(items: List(String)) -> String {
  case items {
    [] -> ""
    _ ->
      list.map(items, fn(v) { string.concat(["- ", v, "\n"]) }) |> string.concat
  }
}

// === NOTIFICATION ===

fn maybe_notify_success(
  input_json: String,
  brief_id: String,
  brief_json: String,
  dev_output: String,
  dev_enrichments: List(String),
  review_output: String,
  review_enrichments: List(String),
) -> Nil {
  case ffi.json_opt(input_json, "notify", "") {
    Ok("") -> Nil
    Error(_) -> Nil
    Ok(notify) -> {
      let run_name = ffi.json_opt(input_json, "run-name", brief_id) |> result.unwrap(brief_id)
      let report_text = report.build_report(
        brief_json, dev_output, dev_enrichments, review_output, review_enrichments,
      )
      let _ = ffi.write_file(".report.tmp", report_text)
      let _ = ffi.run_cmd(string.concat([
        "collective send --as Meridian --to '", notify,
        "' --subject 'workflow complete: ", run_name,
        "' --message \"$(cat .report.tmp)\"",
      ]))
      let _ = ffi.run_cmd("rm -f .report.tmp")
      Nil
    }
  }
}

fn maybe_notify_failure(input_json: String, reason: String) -> Nil {
  case ffi.json_opt(input_json, "notify", "") {
    Ok("") -> Nil
    Error(_) -> Nil
    Ok(notify) -> {
      let brief_id = case ffi.json_get(input_json, "brief") {
        Ok(brief) -> ffi.json_opt(brief, "id", "unknown") |> result.unwrap("unknown")
        Error(_) -> "unknown"
      }
      let run_name = ffi.json_opt(input_json, "run-name", brief_id) |> result.unwrap(brief_id)
      let _ = ffi.run_cmd(string.concat([
        "collective send --as Meridian --to '", notify,
        "' --subject 'workflow FAILED: ", run_name,
        "' --message 'onatopp-dev-gleam failed for ", brief_id, ": ", reason, "'",
      ]))
      Nil
    }
  }
}
