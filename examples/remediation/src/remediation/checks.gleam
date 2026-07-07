//// Pure mechanical checks and projections over stage artifacts — no effects,
//// no engine, no I/O; exhaustively unit-tested (`test/checks_test`).
////
//// These are the workflow-side halves of the DESIGN.md gates (2026-07-07
//// contract): Gate 1's coverage/routing rules need the finding CATEGORIES
//// and the manifest's routing fields, which the shell gate does not see — so
//// they run here, on the typed entries and manifest, before the shell
//// re-runs the tests. Gate 2's every-finding-accounted rule (each brief
//// finding in exactly ONE of addressed|bounced) needs the brief's finding
//// ids, so it runs here too. Gate 3's DERIVE-AND-CHECK rule lives here: the
//// verdict's `overall` is derived mechanically from `per_finding` and a
//// verdict asserting a different value (or rejecting without a reason) is
//// itself rejected — consistency is checked, never trusted.

import gleam/list
import gleam/option.{None, Some}
import gleam/set
import gleam/string
import remediation/types.{
  type FixReport, type Gate2Outcome, type LedgerEntry, type ManifestEntry,
  type Overall, type Ruling, type TestManifest, type Verdict, Accept,
  AcceptanceCheck, Correction, Fixed, Gate1Check, NotFixed, PartialAccept,
  RegressionIntroduced, Reject,
}

// --- gate 1: coverage and routing ---------------------------------------------

/// Gate 1 coverage (mechanical, DESIGN.md Stage 1): every CORRECTION finding
/// in the brief's entries must have a manifest entry with at least one test
/// name or an explicit `could_not_reproduce` flag. `manual_acceptance` does
/// NOT cover a correction — the contract reserves it for improvement/
/// completion findings. Returns the uncovered finding ids — an empty list is
/// a pass. A missing manifest entry, an entry with neither tests nor the
/// flag: both are uncovered (a silently dropped finding is exactly what this
/// forbids).
pub fn uncovered_corrections(
  entries: List(LedgerEntry),
  manifest: TestManifest,
) -> List(String) {
  entries
  |> list.filter(fn(entry) { entry.category == Correction })
  |> list.filter_map(fn(entry) {
    case correction_covered(entry.id, manifest) {
      True -> Error(Nil)
      False -> Ok(entry.id)
    }
  })
}

fn correction_covered(finding_id: String, manifest: TestManifest) -> Bool {
  list.any(manifest.entries, fn(manifest_entry) {
    manifest_entry.finding_id == finding_id
    && {
      manifest_entry.could_not_reproduce
      || !list.is_empty(manifest_entry.test_names)
    }
  })
}

/// Manifest entries with NO evidence channel at all — no tests, no
/// `could_not_reproduce`, no substantive `manual_acceptance`. Such an entry
/// could be verified by nobody downstream, so it fails gate 1's workflow half
/// rather than silently vanishing from the mechanical gate.
pub fn unroutable_entries(manifest: TestManifest) -> List(String) {
  manifest.entries
  |> list.filter(fn(entry) {
    !entry.could_not_reproduce
    && list.is_empty(entry.test_names)
    && !has_manual_acceptance(entry)
  })
  |> list.map(fn(entry) { entry.finding_id })
}

/// Runnable manifest entries whose `expected_failure_signature` is empty —
/// the schema reserves an empty signature for manual-acceptance entries, and
/// without one gate 1's fails-for-the-RIGHT-reason check would be vacuous
/// (every output contains the empty string). A workflow-half gate-1 failure.
pub fn missing_signatures(manifest: TestManifest) -> List(String) {
  manifest.entries
  |> list.filter(fn(entry) {
    !entry.could_not_reproduce
    && !list.is_empty(entry.test_names)
    && string.is_empty(string.trim(entry.expected_failure_signature))
  })
  |> list.map(fn(entry) { entry.finding_id })
}

/// The finding ids the test-author flagged `could_not_reproduce` — carried
/// through to the brief result for the operator (DECISIONS.md D4: no
/// automated reroute in Wave 0).
pub fn could_not_reproduce_ids(manifest: TestManifest) -> List(String) {
  manifest.entries
  |> list.filter(fn(entry) { entry.could_not_reproduce })
  |> list.map(fn(entry) { entry.finding_id })
}

/// The RUNNABLE gate-1 checks: entries with tests, not flagged
/// `could_not_reproduce`, in manifest order. Each carries its finding id and
/// failure signature so the shell's re-run is fully mechanical.
pub fn runnable_checks(manifest: TestManifest) -> List(types.Gate1Check) {
  manifest.entries
  |> list.filter(fn(entry) {
    !entry.could_not_reproduce && !list.is_empty(entry.test_names)
  })
  |> list.map(fn(entry) {
    Gate1Check(
      finding_id: entry.finding_id,
      test_names: entry.test_names,
      expected_failure_signature: entry.expected_failure_signature,
    )
  })
}

/// The MANUAL-ACCEPTANCE gate-1 entries (improvement/completion findings
/// with no expressible failing test): nothing to run; the criterion is echoed
/// through the gate result for the verifier. An entry with tests is runnable,
/// never manual — the criterion channel applies only where nothing runs.
pub fn acceptance_checks(
  manifest: TestManifest,
) -> List(types.AcceptanceCheck) {
  manifest.entries
  |> list.filter(fn(entry) {
    !entry.could_not_reproduce
    && list.is_empty(entry.test_names)
    && has_manual_acceptance(entry)
  })
  |> list.map(fn(entry) {
    AcceptanceCheck(
      finding_id: entry.finding_id,
      criterion: manual_acceptance_text(entry),
    )
  })
}

/// The manifest's authored test files (non-empty `test_file` values, deduped,
/// manifest order) — the explicitly-allowed set for gate 1's diff-scope check
/// alongside the shared test-path rule.
pub fn test_files(manifest: TestManifest) -> List(String) {
  manifest.entries
  |> list.map(fn(entry) { entry.test_file })
  |> list.filter(fn(path) { !string.is_empty(string.trim(path)) })
  |> dedup_keeping_order
}

fn has_manual_acceptance(entry: ManifestEntry) -> Bool {
  !string.is_empty(string.trim(manual_acceptance_text(entry)))
}

fn manual_acceptance_text(entry: ManifestEntry) -> String {
  case entry.manual_acceptance {
    Some(text) -> text
    None -> ""
  }
}

// --- gate 2: every finding accounted, exactly once ------------------------------

/// Gate 2 accounting (2026-07-07 contract): every brief finding id must
/// appear in EXACTLY ONE of the fix report's `findings_addressed` |
/// `findings_bounced`. Returns one violation line per fault — missing from
/// both, or claimed by both (including a duplicate within one list). Ids the
/// report mentions beyond the brief are not this check's concern (class
/// instances have their own field).
pub fn accounting_violations(
  finding_ids: List(String),
  report: FixReport,
) -> List(String) {
  let addressed =
    list.map(report.findings_addressed, fn(fix) { fix.finding_id })
  let bounced =
    list.map(report.findings_bounced, fn(bounce) { bounce.finding_id })
  list.filter_map(finding_ids, fn(finding_id) {
    let mentions =
      list.length(list.filter(addressed, fn(id) { id == finding_id }))
      + list.length(list.filter(bounced, fn(id) { id == finding_id }))
    case mentions {
      1 -> Error(Nil)
      0 ->
        Ok(finding_id <> ": in neither findings_addressed nor findings_bounced")
      _ ->
        Ok(
          finding_id
          <> ": accounted more than once across findings_addressed/findings_bounced",
        )
    }
  })
}

/// A gate-2 outcome's failure SIGNATURE (Change 1, 2026-07-07 incident
/// W0-B2): a stable string built purely from fields [`Gate2Outcome`] already
/// carries — its diagnostics (which finding/check failed and why) folded
/// together with its diff (the developer's full change since the tests
/// commit). Two consecutive failures with the IDENTICAL signature mean BOTH
/// "the same error" AND "no diff progress" at once: the diagnostics alone
/// could match by coincidence across genuinely different attempts, but a
/// byte-identical diff on top of it means literally nothing changed. Used by
/// the fix-cycle loop to abort early instead of burning the whole cycle
/// budget on a loop that cannot self-correct (`remediation_brief.drive`,
/// `remediation/cycle.on_gate2`).
pub fn gate2_failure_signature(outcome: Gate2Outcome) -> String {
  "diagnostics:"
  <> string.trim(outcome.diagnostics)
  <> "||diff:"
  <> string.trim(outcome.diff)
}

// --- gate 3: derive-and-check ------------------------------------------------------

/// Derive the verdict's `overall` mechanically from its rulings: `Accept` iff
/// every ruling is `fixed`; `Reject` if any ruling is `not_fixed` or
/// `regression_introduced`; otherwise `PartialAccept` (some `partial`, none
/// worse). An EMPTY ruling set derives `Reject`: a verdict that ruled nothing
/// proved nothing (the schema forbids it anyway; totality here must not
/// invent an acceptance).
pub fn derive_overall(verdict: Verdict) -> Overall {
  case verdict.per_finding {
    [] -> Reject
    rulings -> {
      let any_rejecting =
        list.any(rulings, fn(ruling) {
          ruling.ruling == NotFixed || ruling.ruling == RegressionIntroduced
        })
      let all_fixed = list.all(rulings, fn(ruling) { ruling.ruling == Fixed })
      case any_rejecting, all_fixed {
        True, _ -> Reject
        False, True -> Accept
        False, False -> PartialAccept
      }
    }
  }
}

/// The verdict-consistency violations (derive-and-check): the asserted
/// `overall` must equal the derived one, and `reject_reason` must be
/// substantive unless the overall is `accept`. A non-empty result is treated
/// like a gate failure feeding the fix-cycle loop — NEVER a silent acceptance
/// of either the asserted or the derived value.
pub fn verdict_issues(verdict: Verdict) -> List(String) {
  let derived = derive_overall(verdict)
  let mismatch = case verdict.overall == derived {
    True -> []
    False -> [
      "verifier asserted overall="
      <> types.overall_to_string(verdict.overall)
      <> " but the rulings derive "
      <> types.overall_to_string(derived),
    ]
  }
  let reason_missing = case verdict.overall {
    Accept -> []
    _ ->
      case substantive(verdict.reject_reason) {
        True -> []
        False -> [
          "overall="
          <> types.overall_to_string(verdict.overall)
          <> " requires a non-empty reject_reason",
        ]
      }
  }
  list.append(mismatch, reason_missing)
}

fn substantive(value: option.Option(String)) -> Bool {
  case value {
    Some(text) -> !string.is_empty(string.trim(text))
    None -> False
  }
}

/// Gate 3 acceptance: the ONE source of truth for the fix-cycle loop
/// decision. The brief is accepted only when the DERIVED overall is `Accept`
/// AND the verdict is internally consistent ([`verdict_issues`] empty) —
/// otherwise the cycle machinery loops back to the developer with the verdict
/// attached.
pub fn verdict_accepts(verdict: Verdict) -> Bool {
  derive_overall(verdict) == Accept && list.is_empty(verdict_issues(verdict))
}

/// The adverse rulings of a verdict (everything not `fixed`), rendered as
/// `finding_id: ruling` lines for summaries.
pub fn adverse_rulings(verdict: Verdict) -> List(String) {
  verdict.per_finding
  |> list.filter(fn(ruling) { ruling.ruling != Fixed })
  |> list.map(fn(ruling) {
    ruling.finding_id <> ": " <> types.ruling_to_string(ruling.ruling)
  })
}

/// Render a ruling for summaries.
pub fn ruling_word(ruling: Ruling) -> String {
  types.ruling_to_string(ruling)
}

// --- misc helpers ----------------------------------------------------------------

/// Reduce an id to branch-safe characters (letters, digits, `-`, `_`), so a
/// brief id can never mint an invalid git ref name.
pub fn branch_safe(id: String) -> String {
  id
  |> string.to_graphemes
  |> list.map(fn(character) {
    case is_branch_safe(character) {
      True -> character
      False -> "-"
    }
  })
  |> string.join("")
}

fn is_branch_safe(character: String) -> Bool {
  case character {
    "-" | "_" -> True
    _ ->
      string.contains(
        "abcdefghijklmnopqrstuvwxyz0123456789",
        string.lowercase(character),
      )
      && string.length(character) == 1
  }
}

fn dedup_keeping_order(items: List(String)) -> List(String) {
  items
  |> list.fold(#(set.new(), []), fn(acc, item) {
    let #(seen, kept) = acc
    case set.contains(seen, item) {
      True -> acc
      False -> #(set.insert(seen, item), [item, ..kept])
    }
  })
  |> fn(acc) { list.reverse(acc.1) }
}
