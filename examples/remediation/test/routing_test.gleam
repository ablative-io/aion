//// Routing contract tests. The aion server routes a pushed activity to a
//// worker connection by (namespace, task_queue, node) ONLY — never by
//// activity type — and the remediation worker opens FIVE connections on the
//// one `remediation` queue. Every activity the workflow builds must
//// therefore pin BOTH the queue and the node of the one connection that
//// serves it: the five shell activities pin `shell_node`; each driven-agent
//// activity pins its role's node. An unpinned (or mispinned) activity
//// round-robins onto a connection with no handler for it, never reports, and
//// dies at the heartbeat window as a lost-worker failure — the exact defect
//// these tests guard against.

import aion/activity
import gleam/list
import gleam/option.{None, Some}
import gleeunit/should
import remediation/activities
import remediation/types

fn brief() -> types.Brief {
  types.Brief(
    id: "B-1",
    finding_ids: ["YG-268"],
    root_cause: "the None-strategy teardown path lacks a dirty check",
    files_expected: ["crates/yg-core/src/teardown.rs"],
    boundaries: [],
    acceptance: [],
    wave: 0,
    deep_cluster: False,
  )
}

fn manifest() -> types.TestManifest {
  types.TestManifest(brief_id: "B-1", entries: [])
}

fn fix_report() -> types.FixReport {
  types.FixReport(
    brief_id: "B-1",
    commits: [],
    findings_addressed: [],
    findings_bounced: [],
    deviations: [],
    new_tests: [],
    class_instances_found: [],
  )
}

/// Assert one built activity carries the remediation queue AND the expected
/// node pin — the full routing key the server matches a connection on.
fn assert_route(built: activity.Activity(i, o), node: String) -> Nil {
  activity.selected_task_queue(built)
  |> should.equal(Some(activities.task_queue))
  activity.selected_node(built)
  |> should.equal(Some(node))
}

// --- agent activities pin their role's node -----------------------------------

pub fn test_author_pins_its_role_node_test() {
  activities.test_author(types.TestAuthorInput(brief: brief(), entries: []))
  |> assert_route(activities.test_author_node)
}

pub fn developer_pins_its_role_node_test() {
  activities.developer(types.DeveloperInput(
    brief: brief(),
    entries: [],
    manifest: manifest(),
    gate1_results: [],
    verdict: None,
    gate2: None,
  ))
  |> assert_route(activities.developer_node)
}

pub fn verifier_pins_its_role_node_test() {
  activities.verifier(types.VerifierInput(
    brief: brief(),
    entries: [],
    manifest: manifest(),
    fix_report: fix_report(),
    diff: "",
  ))
  |> assert_route(activities.verifier_node)
}

// --- shell activities all pin the shell node -----------------------------------

pub fn provision_pins_the_shell_node_test() {
  activities.provision(types.ProvisionInput(
    repo_root: ".",
    base_branch: "main",
    branch: "b1/wave-0",
    workspace_path: "/tmp/ws",
  ))
  |> assert_route(activities.shell_node)
}

pub fn gate1_pins_the_shell_node_test() {
  activities.gate1(
    types.Gate1Input(
      workspace_path: "/tmp/ws",
      base_commit: "abc123",
      checks: [],
      acceptance: [],
      test_files: [],
    ),
  )
  |> assert_route(activities.shell_node)
}

pub fn gate2_pins_the_shell_node_test() {
  activities.gate2(
    types.Gate2Input(
      workspace_path: "/tmp/ws",
      tests_commit: "abc123",
      authored_test_paths: [],
    ),
  )
  |> assert_route(activities.shell_node)
}

pub fn ledger_update_pins_the_shell_node_test() {
  activities.ledger_update(types.LedgerUpdateInput(
    repo_root: ".",
    ledger_path: "ledger.json",
    kind: types.TestManifestArtifact,
    artifact_json: "{}",
  ))
  |> assert_route(activities.shell_node)
}

pub fn cleanup_pins_the_shell_node_test() {
  activities.cleanup(types.CleanupInput(
    repo_root: ".",
    workspace_path: "/tmp/ws",
  ))
  |> assert_route(activities.shell_node)
}

// --- the node table itself ------------------------------------------------------

/// The five node ids are pairwise distinct — two connections sharing a node
/// would let the server land an activity on the wrong one.
pub fn node_ids_are_pairwise_distinct_test() {
  [
    activities.shell_node,
    activities.test_author_node,
    activities.developer_node,
    activities.verifier_node,
    activities.re_auditor_node,
  ]
  |> list.unique
  |> list.length
  |> should.equal(5)
}

/// Pin the EXACT strings the worker registers (worker/src/main.rs:
/// `SHELL_NODE` and `Role::node()`, which is each role's activity type). The
/// server matches these blindly; a drift on either side silently strands
/// activities on handlerless connections.
pub fn node_ids_pin_the_worker_registration_strings_test() {
  activities.shell_node |> should.equal("shell")
  activities.test_author_node |> should.equal("test_author")
  activities.developer_node |> should.equal("developer")
  activities.verifier_node |> should.equal("verifier")
  activities.re_auditor_node |> should.equal("re_auditor")
}
