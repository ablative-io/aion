//// The DERIVE-AND-CHECK rules for lens verdicts, pure and exhaustively
//// unit-tested (`test/verdicts_test`) — the loop decision never trusts an
//// agent-asserted overall.
////
//// A lens verdict's derived overall is a mechanical function of its
//// findings: any `blocking` finding derives Reject; none derives Accept.
//// The asserted overall must agree, and a rejecting verdict must carry a
//// `reject_reason`. Violations are recorded (cycle-stamped by the workflow)
//// AND treated as a rejection — never a silent acceptance of either value.

import dev_brief/types.{
  type Lens, type LensVerdict, type Overall, Accept, Blocking, Reject,
}
import gleam/list
import gleam/option.{None, Some}
import gleam/string

/// The overall a verdict's findings mechanically derive: any blocking
/// finding rejects.
pub fn derive_overall(verdict: LensVerdict) -> Overall {
  let blocking =
    list.any(verdict.findings, fn(finding) { finding.severity == Blocking })
  case blocking {
    True -> Reject
    False -> Accept
  }
}

/// Every consistency violation in one verdict, each line naming the lens:
/// asserted-vs-derived disagreement, and a rejecting overall (asserted or
/// derived) without a reject_reason.
pub fn verdict_issues(verdict: LensVerdict) -> List(String) {
  let derived = derive_overall(verdict)
  let disagreement = case verdict.overall == derived {
    True -> []
    False -> [
      "lens "
      <> verdict.lens
      <> ": asserted overall `"
      <> types.overall_to_string(verdict.overall)
      <> "` disagrees with the derived `"
      <> types.overall_to_string(derived)
      <> "` (findings are the source of truth)",
    ]
  }
  let missing_reason = case derived == Reject || verdict.overall == Reject {
    True ->
      case verdict.reject_reason {
        Some(reason) ->
          case string.trim(reason) {
            "" -> [
              "lens "
              <> verdict.lens
              <> ": rejecting verdict with an empty "
              <> "reject_reason",
            ]
            _ -> []
          }
        None -> [
          "lens "
          <> verdict.lens
          <> ": rejecting verdict without a "
          <> "reject_reason",
        ]
      }
    False -> []
  }
  list.append(disagreement, missing_reason)
}

/// Whether one verdict accepts under derive-and-check: the DERIVED overall
/// is Accept AND the verdict is internally consistent. An inconsistent
/// verdict never accepts.
pub fn verdict_accepts(verdict: LensVerdict) -> Bool {
  derive_overall(verdict) == Accept && verdict_issues(verdict) == []
}

/// Whether a collected review round accepts: EVERY lens verdict accepts.
/// An empty round never accepts — zero lenses reviewing is a configuration
/// fault, not an approval.
pub fn all_accept(collected: List(LensVerdict)) -> Bool {
  case collected {
    [] -> False
    _ -> list.all(collected, verdict_accepts)
  }
}

/// The adverse evidence lines of a review round, for loop-back diagnostics
/// and the terminal summary: each rejecting lens's reason plus its blocking
/// finding titles.
pub fn adverse_lines(collected: List(LensVerdict)) -> List(String) {
  list.flat_map(collected, fn(verdict) {
    case verdict_accepts(verdict) {
      True -> []
      False -> {
        let reason = case verdict.reject_reason {
          Some(text) -> text
          None -> "(no reject_reason given)"
        }
        let blocking_titles =
          verdict.findings
          |> list.filter(fn(finding) { finding.severity == Blocking })
          |> list.map(fn(finding) { finding.title })
        [
          "lens "
          <> verdict.lens
          <> " rejects: "
          <> reason
          <> case blocking_titles {
            [] -> ""
            titles -> " [blocking: " <> string.join(titles, "; ") <> "]"
          },
        ]
      }
    }
  })
}

/// Every lens name that produced NO verdict in a collected round (a spawned
/// child that failed or was lost) — surfaced as a fault, never silently
/// treated as an acceptance.
pub fn missing_lenses(
  lenses: List(Lens),
  collected: List(LensVerdict),
) -> List(String) {
  let returned = list.map(collected, fn(verdict) { verdict.lens })
  lenses
  |> list.map(fn(lens) { lens.name })
  |> list.filter(fn(name) { !list.contains(returned, name) })
}

/// A branch-name-safe rendering of a brief id: anything outside
/// `[A-Za-z0-9._-]` becomes `-`. Pure and total — a weird id degrades to an
/// ugly branch, never a failed git command.
pub fn branch_safe(id: String) -> String {
  id
  |> string.to_graphemes
  |> list.map(fn(grapheme) {
    case is_branch_safe_grapheme(grapheme) {
      True -> grapheme
      False -> "-"
    }
  })
  |> string.concat
}

fn is_branch_safe_grapheme(grapheme: String) -> Bool {
  case grapheme {
    "." | "_" | "-" -> True
    _ ->
      string.length(grapheme) == 1
      && string.contains(
        "abcdefghijklmnopqrstuvwxyz0123456789",
        string.lowercase(grapheme),
      )
  }
}
