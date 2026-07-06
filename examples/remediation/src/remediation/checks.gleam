//// Pure mechanical checks and projections over stage artifacts — no effects,
//// no engine, no I/O; exhaustively unit-tested (`test/checks_test`).
////
//// These are the workflow-side halves of the DESIGN.md gates: Gate 1's
//// coverage rule (every correction finding has >= 1 authored test or an
//// explicit `could_not_reproduce` flag) needs the finding CATEGORIES, which
//// the shell gate does not see — so it runs here, on the typed entries and
//// manifest, before the shell re-runs the tests. Gate 3's acceptance rule
//// (every ruling `fixed`) and the D4 `could_not_reproduce` surfacing are the
//// same shape.

import gleam/list
import gleam/string
import remediation/types.{
  type LedgerEntry, type Ruling, type TestManifest, type Verdict, Correction,
  Fixed,
}

/// Gate 1 coverage (mechanical, DESIGN.md Stage 1): every CORRECTION finding
/// in the brief's entries must have a manifest entry with at least one test
/// name or an explicit `could_not_reproduce` flag. Returns the uncovered
/// finding ids — an empty list is a pass. A missing manifest entry, an entry
/// with neither tests nor the flag: both are uncovered (a silently dropped
/// finding is exactly what this forbids).
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

/// The finding ids the test-author flagged `could_not_reproduce` — carried
/// through to the brief result for the operator (DECISIONS.md D4: no
/// automated reroute in Wave 0).
pub fn could_not_reproduce_ids(manifest: TestManifest) -> List(String) {
  manifest.entries
  |> list.filter(fn(entry) { entry.could_not_reproduce })
  |> list.map(fn(entry) { entry.finding_id })
}

/// The authored test names gate 1 must re-run: every test of every entry NOT
/// flagged `could_not_reproduce`, in manifest order.
pub fn runnable_tests(manifest: TestManifest) -> List(String) {
  manifest.entries
  |> list.filter(fn(entry) { !entry.could_not_reproduce })
  |> list.flat_map(fn(entry) { entry.test_names })
}

/// Gate 3 acceptance: the verdict accepts the brief only when it rules EVERY
/// finding `fixed`. An empty ruling set is NOT acceptance — a verdict that
/// ruled nothing proved nothing (`verdict.schema.json` requires >= 1 ruling,
/// and this check refuses to promote a vacuous one).
pub fn verdict_accepts(verdict: Verdict) -> Bool {
  !list.is_empty(verdict.per_finding)
  && list.all(verdict.per_finding, fn(ruling) { ruling.ruling == Fixed })
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
