//! Tests for the workflow structure projection.
//!
//! The C23 diff test loads the real `examples/order-saga` workflow source,
//! extracts its structure, and asserts the node/edge set matches the saga's
//! REAL branched control flow — the compensations on the `Error` arms of the
//! reserve / charge / ship `case` expressions, not a linear flatten. The oracle
//! is hand-derived from reading the saga's arms and helper functions, and a flat
//! six-node sequence is asserted to compare *unequal* against the real
//! extraction, so the oracle genuinely distinguishes a branch from a sequence.
//!
//! Vocabulary-coverage tests drive each primitive from inline fixtures (including
//! per-member fan-out for `all` / `race` / `map`), a loud-on-unmodellable test
//! asserts an unresolvable `case` surfaces as an `Opaque` node rather than a
//! false sequential edge, and the C24 round-trip test regenerates Gleam from a
//! Run-only sub-shape and type-checks it with the real `gleam` binary, gated at
//! runtime (it skips with a logged line when `gleam` is absent, never
//! `#[ignore]`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion,
    Package, content_hash,
};

use super::error::StructureError;
use super::extract::extract_structure;
use super::model::{
    ArmLabel, CorrelationKey, EdgeKind, GraphEdge, GraphNode, NodeId, NodePrimitive, WorkflowGraph,
};
use super::regen::{StructuralDelta, regenerate_gleam};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SAGA_ACTIVITIES: &[&str] = &[
    "reserve_inventory",
    "charge_payment",
    "ship_order",
    "release_inventory",
    "refund_payment",
    "cancel_shipment",
];

/// Builds a loadable package whose entry module carries `source`, whose manifest
/// names `entry_function` as the orchestration root, and declares `activities`.
fn package_with(
    entry_module: &str,
    entry_function: &str,
    source: &str,
    activities: &[&str],
) -> Result<Package, String> {
    let beams = BeamSet::new(vec![BeamModule::new(entry_module, vec![1, 2, 3])])
        .map_err(|error| error.to_string())?;
    let hash = content_hash(&beams);
    let manifest = Manifest {
        entry_module: entry_module.to_owned(),
        entry_function: entry_function.to_owned(),
        input_schema: serde_json::json!({ "type": "string" }),
        output_schema: serde_json::json!({ "type": "string" }),
        timeout: std::time::Duration::from_secs(60),
        activities: activities
            .iter()
            .map(|name| DeclaredActivity {
                activity_type: (*name).to_owned(),
            })
            .collect(),
        version: ManifestVersion::new(hash.to_string()),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    };
    let source_map = BTreeMap::from([(entry_module.to_owned(), source.as_bytes().to_vec())]);
    Ok(Package::from_validated_parts_for_test(
        manifest, beams, source_map, hash,
    ))
}

fn workspace_root() -> PathBuf {
    // crates/aion-package -> repo root.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn order_saga_source() -> Result<String, String> {
    let path = workspace_root()
        .join("examples")
        .join("order-saga")
        .join("src")
        .join("order_saga.gleam");
    std::fs::read_to_string(&path).map_err(|error| format!("reading {}: {error}", path.display()))
}

fn saga_package() -> Result<Package, String> {
    // The saga's real entry function is `run` (the engine entry that decodes the
    // input and calls `execute`); the walker follows it into the orchestration.
    let source = order_saga_source()?;
    package_with("order_saga", "run", &source, SAGA_ACTIVITIES)
}

// --- helpers for asserting graph shape --------------------------------------

fn node_at(graph: &WorkflowGraph, id: NodeId) -> &GraphNode {
    graph
        .node(id)
        .unwrap_or_else(|| unreachable!("edge endpoint {id:?} must be a node in the graph"))
}

/// Whether `graph` has a labelled branch edge from `from` to `to`.
fn has_branch_edge(graph: &WorkflowGraph, from: NodeId, to: NodeId, arm: &ArmLabel) -> bool {
    graph.edges().iter().any(|edge| {
        edge.from == from
            && edge.to == to
            && matches!(&edge.kind, EdgeKind::Branch { arm: a } if a == arm)
    })
}

/// Whether some node is a `Run` of `activity` reachable from `from` over a
/// branch edge with the given arm label, i.e. the activity is the head of that
/// arm's subgraph.
fn arm_reaches_run(graph: &WorkflowGraph, from: NodeId, arm: &ArmLabel, activity: &str) -> bool {
    graph.edges().iter().any(|edge| {
        edge.from == from
            && matches!(&edge.kind, EdgeKind::Branch { arm: a } if a == arm)
            && is_run_of(node_at(graph, edge.to), activity)
    })
}

fn is_run_of(node: &GraphNode, activity: &str) -> bool {
    node.primitive == NodePrimitive::Run
        && matches!(
            &node.correlation,
            CorrelationKey::ActivitySequence { activity: a, .. } if a == activity
        )
}

/// The `Run` node for `activity`. The caller asserts presence by construction of
/// the fixture, so absence is an unreachable test-setup error.
fn run_node<'graph>(graph: &'graph WorkflowGraph, activity: &str) -> &'graph GraphNode {
    graph
        .nodes()
        .iter()
        .find(|node| is_run_of(node, activity))
        .unwrap_or_else(|| unreachable!("expected a Run node for `{activity}`"))
}

/// The branch node that the `Run` of `activity` sequences directly into.
fn branch_after_run(graph: &WorkflowGraph, activity: &str) -> NodeId {
    let run = run_node(graph, activity).id;
    let branch = graph
        .edges()
        .iter()
        .find(|edge| edge.from == run && edge.kind == EdgeKind::Sequence)
        .map_or_else(
            || unreachable!("expected a sequence edge out of the `{activity}` run"),
            |edge| edge.to,
        );
    assert_eq!(
        node_at(graph, branch).primitive,
        NodePrimitive::Branch,
        "the node after a scrutinised `{activity}` run must be a Branch"
    );
    branch
}

// --- R1 / C21 / C23: faithful branched structure and the diff ----------------

#[test]
fn order_saga_extracts_faithful_branched_control_flow() -> TestResult {
    let package = saga_package()?;
    let graph = extract_structure(&package)?;

    assert_eq!(graph.entry_module(), "order_saga");

    // Every saga activity dispatched on the success path or a compensation arm
    // appears as a Run node. `cancel_shipment` is a public entry the orchestration
    // never calls, so it is correctly absent.
    for activity in ["reserve_inventory", "charge_payment", "ship_order"] {
        assert!(
            graph.nodes().iter().any(|n| is_run_of(n, activity)),
            "expected a Run node for the success-path activity `{activity}`"
        );
    }
    assert!(
        !graph
            .nodes()
            .iter()
            .any(|n| is_run_of(n, "cancel_shipment")),
        "cancel_shipment is never called by the orchestration; it must not appear"
    );

    // reserve -> Branch: Ok arm reaches the charge run; Error arm reaches no
    // compensation (nothing to compensate before reserve succeeds).
    let reserve_branch = branch_after_run(&graph, "reserve_inventory");
    assert!(
        arm_reaches_run(&graph, reserve_branch, &ArmLabel::Ok, "charge_payment"),
        "reserve's Ok arm must reach the charge run"
    );
    assert!(
        !graph.edges().iter().any(|edge| edge.from == reserve_branch
            && matches!(&edge.kind, EdgeKind::Branch { arm } if *arm == ArmLabel::Error)),
        "reserve's Error arm compensates nothing"
    );

    // charge -> Branch: Ok arm reaches the ship run; Error arm reaches the
    // release_inventory compensation.
    let charge_branch = branch_after_run(&graph, "charge_payment");
    assert!(
        arm_reaches_run(&graph, charge_branch, &ArmLabel::Ok, "ship_order"),
        "charge's Ok arm must reach the ship run"
    );
    assert!(
        arm_reaches_run(&graph, charge_branch, &ArmLabel::Error, "release_inventory"),
        "charge's Error arm must reach the release_inventory compensation"
    );

    // ship -> Branch: Ok arm is the success terminal (no further run); Error arm
    // reaches the refund_payment compensation, which is followed by release.
    let ship_branch = branch_after_run(&graph, "ship_order");
    assert!(
        arm_reaches_run(&graph, ship_branch, &ArmLabel::Error, "refund_payment"),
        "ship's Error arm must reach the refund_payment compensation"
    );
    assert!(
        !arm_reaches_run(&graph, ship_branch, &ArmLabel::Ok, "release_inventory")
            && !arm_reaches_run(&graph, ship_branch, &ArmLabel::Ok, "refund_payment"),
        "ship's Ok arm is the success terminal, not a compensation"
    );

    // The ship-Error compensation chains refund -> release_inventory in sequence.
    let refund = run_node(&graph, "refund_payment").id;
    let refund_branch_edge = graph
        .edges()
        .iter()
        .find(|edge| edge.from == refund && edge.kind == EdgeKind::Sequence);
    assert!(
        refund_branch_edge.is_some(),
        "refund sequences into its own branch"
    );
    let refund_branch =
        refund_branch_edge.map_or_else(|| unreachable!("asserted present above"), |edge| edge.to);
    let reaches_release_after_refund = graph.edges().iter().any(|edge| {
        edge.from == refund_branch
            && edge.kind == EdgeKind::Sequence
            && is_run_of(node_at(&graph, edge.to), "release_inventory")
    });
    assert!(
        reaches_release_after_refund,
        "the ship-Error compensation must run refund then release_inventory"
    );

    Ok(())
}

/// C23: the hand-derived oracle matches the extraction, and a flat six-node
/// sequence does NOT — proving the oracle distinguishes a branch from a flatten.
#[test]
fn c23_oracle_matches_real_branches_and_rejects_a_flat_sequence() -> TestResult {
    let package = saga_package()?;
    let graph = extract_structure(&package)?;

    let oracle = known_saga_graph();
    assert!(
        graph.structurally_equals(&oracle),
        "extracted graph must match the saga's hand-derived branched oracle;\n\
         extracted nodes={} edges={}, oracle nodes={} edges={}",
        graph.nodes().len(),
        graph.edges().len(),
        oracle.nodes().len(),
        oracle.edges().len(),
    );

    // The negative oracle: a flat six-run sequence (what the superseded
    // flatten produced). It MUST compare unequal — if it matched, the oracle
    // would be hand-drawn to a flatten rather than the real branches.
    let flat = flat_six_run_sequence();
    assert!(
        !graph.structurally_equals(&flat),
        "a flat six-node sequence must NOT match the real branched extraction"
    );
    // And the flat sequence must not even have the same cardinality.
    assert_ne!(graph.nodes().len(), flat.nodes().len());

    Ok(())
}

#[test]
fn extraction_is_deterministic() -> TestResult {
    let package = saga_package()?;
    let first = extract_structure(&package)?;
    let second = extract_structure(&package)?;
    assert_eq!(first, second);
    Ok(())
}

/// The saga's known structure, hand-derived from reading `run`, `execute`, and
/// the helper functions — the oracle the extraction is diffed against (C23).
///
/// Tracing the real source:
/// - n0 Run(reserve) -> n1 Branch(reserve): Ok -> n2 Run(charge); Error terminal
/// - n2 Run(charge) -> n3 Branch(charge): Ok -> n4 Run(ship); Error -> n10 Run(release)
/// - n4 Run(ship) -> n5 Branch(ship): Ok terminal; Error -> n6 Run(refund)
/// - n6 Run(refund) -> n7 Branch(refund helper case) -> n8 Run(release) -> n9 Branch
/// - n10 Run(release) -> n11 Branch(release helper case): terminal
/// - every execute exit (n1, n5, n9, n11) feeds n12 Branch(`case execute(input)` in `run`)
fn known_saga_graph() -> WorkflowGraph {
    let nodes = vec![
        run(0, 0, "reserve_inventory"),
        branch(1, 0),
        run(2, 1, "charge_payment"),
        branch(3, 1),
        run(4, 2, "ship_order"),
        branch(5, 2),
        run(6, 3, "refund_payment"),
        branch(7, 3),
        run(8, 4, "release_inventory"),
        branch(9, 4),
        run(10, 5, "release_inventory"),
        branch(11, 5),
        branch(12, 6),
    ];
    let edges = vec![
        seq(0, 1),
        branch_edge(1, 2, ArmLabel::Ok),
        seq(2, 3),
        branch_edge(3, 4, ArmLabel::Ok),
        branch_edge(3, 10, ArmLabel::Error),
        seq(4, 5),
        branch_edge(5, 6, ArmLabel::Error),
        seq(6, 7),
        seq(7, 8),
        seq(8, 9),
        seq(10, 11),
        // Every orchestration exit flows into the `case execute(input)` branch.
        seq(1, 12),
        seq(5, 12),
        seq(9, 12),
        seq(11, 12),
    ];
    WorkflowGraph {
        entry_module: "order_saga".to_owned(),
        nodes,
        edges,
    }
}

/// The negative oracle: the six-node flat sequence the superseded flatten
/// produced. The real extraction must not match this.
fn flat_six_run_sequence() -> WorkflowGraph {
    let nodes = SAGA_ACTIVITIES
        .iter()
        .enumerate()
        .map(|(ordinal, activity)| run(ordinal, ordinal, activity))
        .collect();
    let edges = (0..SAGA_ACTIVITIES.len() - 1)
        .map(|i| seq(i, i + 1))
        .collect();
    WorkflowGraph {
        entry_module: "order_saga".to_owned(),
        nodes,
        edges,
    }
}

fn run(id: usize, ordinal: usize, activity: &str) -> GraphNode {
    GraphNode {
        id: NodeId(id),
        primitive: NodePrimitive::Run,
        correlation: CorrelationKey::ActivitySequence {
            ordinal,
            activity: activity.to_owned(),
        },
    }
}

fn branch(id: usize, ordinal: usize) -> GraphNode {
    GraphNode {
        id: NodeId(id),
        primitive: NodePrimitive::Branch,
        correlation: CorrelationKey::ControlFlow { ordinal },
    }
}

fn seq(from: usize, to: usize) -> GraphEdge {
    GraphEdge {
        from: NodeId(from),
        to: NodeId(to),
        kind: EdgeKind::Sequence,
    }
}

fn branch_edge(from: usize, to: usize, arm: ArmLabel) -> GraphEdge {
    GraphEdge {
        from: NodeId(from),
        to: NodeId(to),
        kind: EdgeKind::Branch { arm },
    }
}

// --- R1 negative paths -------------------------------------------------------

#[test]
fn missing_entry_source_is_a_loud_error() -> TestResult {
    let beams = BeamSet::new(vec![BeamModule::new("order_saga", vec![1, 2, 3])])?;
    let hash = content_hash(&beams);
    let manifest = Manifest {
        entry_module: "order_saga".to_owned(),
        entry_function: "run".to_owned(),
        input_schema: serde_json::json!({}),
        output_schema: serde_json::json!({}),
        timeout: std::time::Duration::from_secs(1),
        activities: vec![],
        version: ManifestVersion::new(hash.to_string()),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    };
    let package = Package::from_validated_parts_for_test(manifest, beams, BTreeMap::new(), hash);

    assert_eq!(
        extract_structure(&package),
        Err(StructureError::MissingEntrySource {
            module: "order_saga".to_owned(),
        })
    );
    Ok(())
}

#[test]
fn non_utf8_entry_source_is_a_loud_error() -> TestResult {
    let beams = BeamSet::new(vec![BeamModule::new("order_saga", vec![1, 2, 3])])?;
    let hash = content_hash(&beams);
    let manifest = Manifest {
        entry_module: "order_saga".to_owned(),
        entry_function: "run".to_owned(),
        input_schema: serde_json::json!({}),
        output_schema: serde_json::json!({}),
        timeout: std::time::Duration::from_secs(1),
        activities: vec![],
        version: ManifestVersion::new(hash.to_string()),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    };
    let source = BTreeMap::from([("order_saga".to_owned(), vec![0xff, 0xfe, 0x00])]);
    let package = Package::from_validated_parts_for_test(manifest, beams, source, hash);

    assert_eq!(
        extract_structure(&package),
        Err(StructureError::EntrySourceNotUtf8 {
            module: "order_saga".to_owned(),
        })
    );
    Ok(())
}

#[test]
fn entry_without_workflow_import_is_a_loud_error() -> TestResult {
    let source = "import gleam/io\npub fn run() { io.println(\"hi\") }\n";
    let package = package_with("plain", "run", source, &[])?;
    assert_eq!(
        extract_structure(&package),
        Err(StructureError::NoWorkflowImport {
            module: "plain".to_owned(),
        })
    );
    Ok(())
}

#[test]
fn missing_entry_function_is_a_loud_error() -> TestResult {
    // Imports the vocabulary but the manifest names an entry function the source
    // does not define: a loud error, never a silent empty graph.
    let source = "import aion/workflow\n\
         pub fn execute(input) { workflow.sleep(input) }\n";
    let package = package_with("demo", "run", source, &[])?;
    assert_eq!(
        extract_structure(&package),
        Err(StructureError::EntryFunctionNotFound {
            module: "demo".to_owned(),
            function: "run".to_owned(),
        })
    );
    Ok(())
}

#[test]
fn run_node_with_undeclared_activity_is_rejected() -> TestResult {
    let source = "import aion/workflow\n\
         import demo_activity_wrappers as wrappers\n\
         pub fn execute(input) {\n\
         \u{20}\u{20}workflow.run(wrappers.ghost_activity(input))\n\
         }\n";
    let package = package_with("demo", "execute", source, &["real"])?;
    assert_eq!(
        extract_structure(&package),
        Err(StructureError::UnknownActivity {
            activity: "ghost".to_owned(),
        })
    );
    Ok(())
}

// --- Vocabulary coverage: each primitive yields the right node and key -------

#[test]
fn each_primitive_is_extracted_with_its_correlation_key() -> TestResult {
    let source = "import aion/workflow\n\
         import demo_activity_wrappers as wrappers\n\
         import aion/duration\n\
         pub fn execute(signal_ref, input) {\n\
         \u{20}\u{20}let _ = workflow.receive(signal_ref)\n\
         \u{20}\u{20}let _ = workflow.sleep(duration.seconds(1))\n\
         \u{20}\u{20}let _ = workflow.start_timer(\"deadline\", duration.seconds(5))\n\
         \u{20}\u{20}workflow.run(wrappers.a_activity(input))\n\
         }\n";
    let package = package_with("demo", "execute", source, &["a"])?;
    let graph = extract_structure(&package)?;

    let primitives: Vec<_> = graph.nodes().iter().map(|node| node.primitive).collect();
    assert_eq!(
        primitives,
        vec![
            NodePrimitive::Receive,
            NodePrimitive::Sleep,
            NodePrimitive::StartTimer,
            NodePrimitive::Run,
        ]
    );
    assert_eq!(
        graph.nodes()[0].correlation,
        CorrelationKey::Signal {
            reference: "signal_ref".to_owned(),
        }
    );
    assert_eq!(
        graph.nodes()[2].correlation,
        CorrelationKey::Timer {
            id: "deadline".to_owned(),
        }
    );
    assert_eq!(
        graph.nodes()[3].correlation,
        CorrelationKey::ActivitySequence {
            ordinal: 0,
            activity: "a".to_owned(),
        }
    );

    // Sequential edges chain the four primitives in order.
    let seqs: Vec<_> = graph
        .edges()
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Sequence)
        .map(|edge| (edge.from, edge.to))
        .collect();
    assert_eq!(
        seqs,
        vec![
            (NodeId(0), NodeId(1)),
            (NodeId(1), NodeId(2)),
            (NodeId(2), NodeId(3)),
        ]
    );
    Ok(())
}

/// The fan-out review item: each `all` / `race` / `map` member is its own Run
/// node carrying its `ActivitySequence` key, grouped under the concurrency node
/// by a `FanOut` edge, so a consumer can overlay each member's recorded
/// `ActivityCompleted` independently.
#[test]
fn fan_out_members_are_individual_run_nodes() -> TestResult {
    let source = "import aion/workflow\n\
         import demo_activity_wrappers as wrappers\n\
         pub fn execute(input) {\n\
         \u{20}\u{20}let _ = workflow.all([wrappers.a_activity(input), wrappers.b_activity(input)])\n\
         \u{20}\u{20}let _ = workflow.race([wrappers.c_activity(input)])\n\
         \u{20}\u{20}workflow.map([input], wrappers.d_activity)\n\
         }\n";
    let package = package_with("demo", "execute", source, &["a", "b", "c", "d"])?;
    let graph = extract_structure(&package)?;

    // The concurrency nodes themselves.
    let all = only_node_of(&graph, NodePrimitive::All);
    let race = only_node_of(&graph, NodePrimitive::Race);
    let map = only_node_of(&graph, NodePrimitive::Map);

    // `all` fans out to two member Run nodes via FanOut edges.
    let all_members: Vec<_> = graph
        .edges()
        .iter()
        .filter(|edge| edge.from == all && edge.kind == EdgeKind::FanOut)
        .map(|edge| node_at(&graph, edge.to))
        .collect();
    assert_eq!(all_members.len(), 2, "`all` has two fan-out members");
    assert!(is_run_of(all_members[0], "a"));
    assert!(is_run_of(all_members[1], "b"));

    // Each member carries its own ActivitySequence key.
    for member in &all_members {
        assert!(matches!(
            member.correlation,
            CorrelationKey::ActivitySequence { .. }
        ));
    }

    // `race` fans out to one member, `map` to one (the mapped activity value).
    assert_eq!(
        graph
            .edges()
            .iter()
            .filter(|edge| edge.from == race && edge.kind == EdgeKind::FanOut)
            .count(),
        1
    );
    let map_member_edge = graph
        .edges()
        .iter()
        .find(|edge| edge.from == map && edge.kind == EdgeKind::FanOut);
    assert!(map_member_edge.is_some(), "map has a fan-out member");
    if let Some(edge) = map_member_edge {
        assert!(is_run_of(node_at(&graph, edge.to), "d"));
    }
    Ok(())
}

/// The single node of the given primitive in `graph`, asserting there is exactly
/// one. Returns its id.
fn only_node_of(graph: &WorkflowGraph, primitive: NodePrimitive) -> NodeId {
    let matches: Vec<_> = graph
        .nodes()
        .iter()
        .filter(|node| node.primitive == primitive)
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one {primitive:?} node, found {}",
        matches.len()
    );
    matches[0].id
}

#[test]
fn fan_out_member_undeclared_activity_is_rejected() -> TestResult {
    let source = "import aion/workflow\n\
         import demo_activity_wrappers as wrappers\n\
         pub fn execute(input) {\n\
         \u{20}\u{20}workflow.all([wrappers.ghost_activity(input)])\n\
         }\n";
    let package = package_with("demo", "execute", source, &["real"])?;
    assert_eq!(
        extract_structure(&package),
        Err(StructureError::UnknownActivity {
            activity: "ghost".to_owned(),
        })
    );
    Ok(())
}

#[test]
fn spawn_carries_child_name_and_ordinal() -> TestResult {
    let source = "import aion/workflow\n\
         pub fn execute(child_fn, input, codec) {\n\
         \u{20}\u{20}let _ = workflow.spawn(\"settle\", child_fn, input, codec, codec, codec)\n\
         \u{20}\u{20}workflow.spawn_and_wait(\"finalise\", child_fn, input, codec, codec, codec)\n\
         }\n";
    let package = package_with("demo", "execute", source, &[])?;
    let graph = extract_structure(&package)?;

    assert_eq!(
        graph.nodes()[0].correlation,
        CorrelationKey::Child {
            ordinal: 0,
            name: "settle".to_owned(),
        }
    );
    assert_eq!(graph.nodes()[0].primitive, NodePrimitive::Spawn);
    assert_eq!(
        graph.nodes()[1].correlation,
        CorrelationKey::Child {
            ordinal: 1,
            name: "finalise".to_owned(),
        }
    );
    assert_eq!(graph.nodes()[1].primitive, NodePrimitive::SpawnAndWait);
    Ok(())
}

#[test]
fn aliased_workflow_import_is_supported() -> TestResult {
    let source = "import aion/workflow as wf\n\
         import demo_activity_wrappers as wrappers\n\
         pub fn execute(input) {\n\
         \u{20}\u{20}wf.run(wrappers.a_activity(input))\n\
         }\n";
    let package = package_with("demo", "execute", source, &["a"])?;
    let graph = extract_structure(&package)?;
    assert_eq!(graph.nodes().len(), 1);
    assert_eq!(graph.nodes()[0].primitive, NodePrimitive::Run);
    Ok(())
}

// --- Faithful branching on a primitive `case` and loud-on-unmodellable -------

/// A `case workflow.run(...) { Ok -> ... Error -> ... }` mints a Branch with
/// labelled Ok/Error edges into the real subgraphs — not a flat sequence.
#[test]
fn primitive_case_mints_a_labelled_branch() -> TestResult {
    let source = "import aion/workflow\n\
         import demo_activity_wrappers as wrappers\n\
         pub fn execute(input) {\n\
         \u{20}\u{20}case workflow.run(wrappers.a_activity(input)) {\n\
         \u{20}\u{20}\u{20}\u{20}Ok(v) -> workflow.run(wrappers.b_activity(v))\n\
         \u{20}\u{20}\u{20}\u{20}Error(e) -> workflow.run(wrappers.c_activity(e))\n\
         \u{20}\u{20}}\n\
         }\n";
    let package = package_with("demo", "execute", source, &["a", "b", "c"])?;
    let graph = extract_structure(&package)?;

    let branch = branch_after_run(&graph, "a");
    let b = run_node(&graph, "b").id;
    let c = run_node(&graph, "c").id;
    assert!(has_branch_edge(&graph, branch, b, &ArmLabel::Ok));
    assert!(has_branch_edge(&graph, branch, c, &ArmLabel::Error));

    // The forbidden flatten: there is no direct sequence edge from the `a` run to
    // either arm's run; the only out-edges of `a` go to its Branch.
    let a = run_node(&graph, "a").id;
    for edge in graph.edges() {
        if edge.from == a {
            assert_eq!(
                edge.kind,
                EdgeKind::Sequence,
                "`a`'s only out-edge is the sequence into its branch"
            );
            assert_eq!(edge.to, branch);
        }
    }
    Ok(())
}

/// Loud-on-unmodellable: a `case` the walker cannot bound (no arm body it can
/// delimit before the function ends) surfaces as an explicit `Opaque` node
/// carrying the snippet — never a silent drop and never a false sequence.
#[test]
fn unbounded_case_surfaces_as_an_opaque_node() -> TestResult {
    // The `case` has no `{ ... }` arm body the walker can bound before the
    // function's own closing brace, so it cannot be modelled as a branch.
    let source = "import aion/workflow\n\
         import demo_activity_wrappers as wrappers\n\
         pub fn execute(input) {\n\
         \u{20}\u{20}workflow.run(wrappers.a_activity(input))\n\
         \u{20}\u{20}case input\n\
         }\n";
    let package = package_with("demo", "execute", source, &["a"])?;
    let graph = extract_structure(&package)?;

    let opaque = graph
        .nodes()
        .iter()
        .find(|node| node.primitive == NodePrimitive::Opaque);
    assert!(
        opaque.is_some(),
        "an unbounded case must surface as an Opaque node"
    );
    let Some(opaque) = opaque else {
        return Ok(());
    };
    assert!(matches!(
        &opaque.correlation,
        CorrelationKey::Opaque { snippet, .. } if !snippet.is_empty()
    ));

    // No edge silently flattens the unmodellable case into a false continuation:
    // the only edge to the opaque node is the sequence from the prior run.
    let into_opaque: Vec<_> = graph
        .edges()
        .iter()
        .filter(|edge| edge.to == opaque.id)
        .collect();
    assert!(
        into_opaque
            .iter()
            .all(|edge| edge.kind == EdgeKind::Sequence),
        "the opaque node is reached by an honest sequence edge, not a fabricated branch"
    );
    Ok(())
}

/// A data-driven fork (a `case` over a non-primitive value with two
/// primitive-bearing arms) is a genuine branch and is modelled as one.
#[test]
fn data_case_with_two_primitive_arms_is_a_branch() -> TestResult {
    let source = "import aion/workflow\n\
         import demo_activity_wrappers as wrappers\n\
         pub fn execute(flag, input) {\n\
         \u{20}\u{20}case flag {\n\
         \u{20}\u{20}\u{20}\u{20}True -> workflow.run(wrappers.a_activity(input))\n\
         \u{20}\u{20}\u{20}\u{20}False -> workflow.run(wrappers.b_activity(input))\n\
         \u{20}\u{20}}\n\
         }\n";
    let package = package_with("demo", "execute", source, &["a", "b"])?;
    let graph = extract_structure(&package)?;

    let branch = only_node_of(&graph, NodePrimitive::Branch);
    let a = run_node(&graph, "a").id;
    let b = run_node(&graph, "b").id;
    assert!(graph.edges().iter().any(|edge| edge.from == branch
        && edge.to == a
        && matches!(edge.kind, EdgeKind::Branch { .. })));
    assert!(graph.edges().iter().any(|edge| edge.from == branch
        && edge.to == b
        && matches!(edge.kind, EdgeKind::Branch { .. })));
    Ok(())
}

/// A primitive-free helper call (e.g. an error formatter with its own `case`) is
/// pruned: it contributes no false node or branch to the workflow's primitive
/// structure.
#[test]
fn primitive_free_helper_is_pruned() -> TestResult {
    let source = "import aion/workflow\n\
         import demo_activity_wrappers as wrappers\n\
         pub fn execute(input) {\n\
         \u{20}\u{20}case workflow.run(wrappers.a_activity(input)) {\n\
         \u{20}\u{20}\u{20}\u{20}Ok(v) -> Ok(v)\n\
         \u{20}\u{20}\u{20}\u{20}Error(e) -> Error(describe(e))\n\
         \u{20}\u{20}}\n\
         }\n\
         fn describe(e) {\n\
         \u{20}\u{20}case e {\n\
         \u{20}\u{20}\u{20}\u{20}Bad -> \"bad\"\n\
         \u{20}\u{20}\u{20}\u{20}_ -> \"other\"\n\
         \u{20}\u{20}}\n\
         }\n";
    let package = package_with("demo", "execute", source, &["a"])?;
    let graph = extract_structure(&package)?;

    // Exactly one Run (a) and one Branch (a's case). `describe`'s internal case
    // carries no primitive and must not appear.
    let runs = graph
        .nodes()
        .iter()
        .filter(|n| n.primitive == NodePrimitive::Run)
        .count();
    let branches = graph
        .nodes()
        .iter()
        .filter(|n| n.primitive == NodePrimitive::Branch)
        .count();
    assert_eq!(runs, 1, "only the `a` run is a workflow primitive");
    assert_eq!(branches, 1, "only `a`'s case is a durable branch");
    Ok(())
}

// --- The workflow.entrypoint shim: define-follow extraction -------------------

/// The migrated entry shape (`examples/stacked-dev/src/gate.gleam`): `run` is
/// the one-line `workflow.entrypoint(definition(), raw_input)` shim, and the
/// orchestration is reachable only through the `define(...)` call's typed
/// entry argument.
const SHIM_STYLE: &str = "import aion/workflow\n\
     import demo_activity_wrappers as wrappers\n\
     pub fn definition() {\n\
     \u{20}\u{20}workflow.define(\"demo\", codecs.a(), codecs.b(), codecs.c(), execute)\n\
     }\n\
     pub fn run(raw_input) {\n\
     \u{20}\u{20}workflow.entrypoint(definition(), raw_input)\n\
     }\n\
     pub fn execute(input) {\n\
     \u{20}\u{20}case workflow.run(wrappers.full_checks_activity(input)) {\n\
     \u{20}\u{20}\u{20}\u{20}Ok(result) -> Ok(result)\n\
     \u{20}\u{20}\u{20}\u{20}Error(e) -> Error(describe(e))\n\
     \u{20}\u{20}}\n\
     }\n\
     fn describe(e) {\n\
     \u{20}\u{20}\"failed\"\n\
     }\n";

/// The same orchestration with `run` calling the typed entry directly — the
/// pre-shim adapter's reachable structure.
const DIRECT_STYLE: &str = "import aion/workflow\n\
     import demo_activity_wrappers as wrappers\n\
     pub fn run(input) {\n\
     \u{20}\u{20}execute(input)\n\
     }\n\
     pub fn execute(input) {\n\
     \u{20}\u{20}case workflow.run(wrappers.full_checks_activity(input)) {\n\
     \u{20}\u{20}\u{20}\u{20}Ok(result) -> Ok(result)\n\
     \u{20}\u{20}\u{20}\u{20}Error(e) -> Error(describe(e))\n\
     \u{20}\u{20}}\n\
     }\n\
     fn describe(e) {\n\
     \u{20}\u{20}\"failed\"\n\
     }\n";

/// The equivalence proof for the extractor's define-follow arm: a shim-style
/// entry (`workflow.entrypoint(definition(), raw_input)`) yields the identical
/// graph the direct-call style yields — never an empty graph.
#[test]
fn shim_style_entry_yields_the_same_graph_as_direct_style() -> TestResult {
    let shim = package_with("demo", "run", SHIM_STYLE, &["full_checks"])?;
    let direct = package_with("demo", "run", DIRECT_STYLE, &["full_checks"])?;

    let shim_graph = extract_structure(&shim)?;
    let direct_graph = extract_structure(&direct)?;

    assert!(
        !shim_graph.nodes().is_empty(),
        "the shim-style entry must not extract an empty graph"
    );
    assert!(
        shim_graph.structurally_equals(&direct_graph),
        "shim-style extraction must match direct-style;\n\
         shim nodes={} edges={}, direct nodes={} edges={}",
        shim_graph.nodes().len(),
        shim_graph.edges().len(),
        direct_graph.nodes().len(),
        direct_graph.edges().len(),
    );

    // The concrete shape: the one Run node plus the Branch its `case` mints.
    assert!(
        shim_graph
            .nodes()
            .iter()
            .any(|node| is_run_of(node, "full_checks")),
        "the shim graph carries the full_checks Run node"
    );
    assert_eq!(
        branch_after_run(&shim_graph, "full_checks"),
        branch_after_run(&direct_graph, "full_checks"),
    );
    Ok(())
}

/// A `define` whose last argument is not a bare identifier (a qualified
/// reference) is a no-op walk: no error, no false node.
#[test]
fn define_with_non_bare_last_argument_is_a_no_op_walk() -> TestResult {
    let source = "import aion/workflow\n\
         pub fn definition() {\n\
         \u{20}\u{20}workflow.define(\"demo\", codecs.a(), codecs.b(), codecs.c(), helpers.execute)\n\
         }\n\
         pub fn run(raw_input) {\n\
         \u{20}\u{20}workflow.entrypoint(definition(), raw_input)\n\
         }\n";
    let package = package_with("demo", "run", source, &[])?;
    let graph = extract_structure(&package)?;
    assert!(
        graph.nodes().is_empty() && graph.edges().is_empty(),
        "an unresolvable define entry extracts an empty region without error"
    );
    Ok(())
}

/// The documented double-walk semantics: an entry that BOTH calls its typed
/// entry directly AND reaches `define(...)` walks the entry twice, so the
/// subgraph appears twice. No real module has this shape; this test pins the
/// chosen semantics so a future change is deliberate.
#[test]
fn entry_reaching_execute_directly_and_through_define_walks_it_twice() -> TestResult {
    let source = "import aion/workflow\n\
         import demo_activity_wrappers as wrappers\n\
         pub fn definition() {\n\
         \u{20}\u{20}workflow.define(\"demo\", codecs.a(), codecs.b(), codecs.c(), execute)\n\
         }\n\
         pub fn run(raw_input) {\n\
         \u{20}\u{20}let _ = execute(raw_input)\n\
         \u{20}\u{20}workflow.entrypoint(definition(), raw_input)\n\
         }\n\
         pub fn execute(input) {\n\
         \u{20}\u{20}workflow.run(wrappers.full_checks_activity(input))\n\
         }\n";
    let package = package_with("demo", "run", source, &["full_checks"])?;
    let graph = extract_structure(&package)?;
    let runs = graph
        .nodes()
        .iter()
        .filter(|node| is_run_of(node, "full_checks"))
        .count();
    assert_eq!(
        runs, 2,
        "the direct call and the define-follow each contribute the run subgraph"
    );
    Ok(())
}

/// The recursion guard holds when `define(...)` names the entry function
/// itself: the follow yields an empty region instead of recursing forever.
#[test]
fn define_naming_the_entry_function_is_recursion_guarded() -> TestResult {
    let source = "import aion/workflow\n\
         import demo_activity_wrappers as wrappers\n\
         pub fn run(raw_input) {\n\
         \u{20}\u{20}let _ = workflow.run(wrappers.full_checks_activity(raw_input))\n\
         \u{20}\u{20}workflow.entrypoint(workflow.define(\"demo\", codecs.a(), codecs.b(), codecs.c(), run), raw_input)\n\
         }\n";
    let package = package_with("demo", "run", source, &["full_checks"])?;
    let graph = extract_structure(&package)?;
    // The direct run is extracted once; the self-referential define follow is
    // cut by the stack and contributes nothing.
    let runs = graph
        .nodes()
        .iter()
        .filter(|node| is_run_of(node, "full_checks"))
        .count();
    assert_eq!(runs, 1);
    Ok(())
}

/// The migrated gate.gleam's `aion/workflow as workflow`-default import plus
/// aliased import both reach the entry through `define`.
#[test]
fn aliased_import_shim_style_is_followed() -> TestResult {
    let source = "import aion/workflow as wf\n\
         import demo_activity_wrappers as wrappers\n\
         pub fn definition() {\n\
         \u{20}\u{20}wf.define(\"demo\", codecs.a(), codecs.b(), codecs.c(), execute)\n\
         }\n\
         pub fn run(raw_input) {\n\
         \u{20}\u{20}wf.entrypoint(definition(), raw_input)\n\
         }\n\
         pub fn execute(input) {\n\
         \u{20}\u{20}wf.run(wrappers.full_checks_activity(input))\n\
         }\n";
    let package = package_with("demo", "run", source, &["full_checks"])?;
    let graph = extract_structure(&package)?;
    assert_eq!(graph.nodes().len(), 1);
    assert!(is_run_of(&graph.nodes()[0], "full_checks"));
    Ok(())
}

// --- R2 / C24: bounded regeneration -----------------------------------------

/// A Run-only sub-shape over the saga's declared activities — the bounded
/// round-trip's emittable shape. Built directly (not extracted) because the real
/// branched extraction carries Branch nodes the round-trip deliberately refuses.
fn run_only_graph() -> WorkflowGraph {
    let nodes = vec![
        run(0, 0, "reserve_inventory"),
        run(1, 1, "charge_payment"),
        run(2, 2, "ship_order"),
    ];
    let edges = vec![seq(0, 1), seq(1, 2)];
    WorkflowGraph {
        entry_module: "order_saga".to_owned(),
        nodes,
        edges,
    }
}

#[test]
fn branched_graph_is_refused_by_the_bounded_round_trip() -> TestResult {
    // The real branched extraction carries Branch nodes; the round-trip
    // regenerates `run` chains only and refuses anything else loudly.
    let package = saga_package()?;
    let graph = extract_structure(&package)?;
    let delta = StructuralDelta::AppendRun {
        activity: "charge_payment".to_owned(),
        after: NodeId(0),
    };
    assert!(matches!(
        regenerate_gleam(&package, &graph, &delta),
        Err(StructureError::UnboundedDelta { .. })
    ));
    Ok(())
}

#[test]
fn append_run_unknown_activity_is_refused() -> TestResult {
    let package = saga_package()?;
    let graph = run_only_graph();
    let delta = StructuralDelta::AppendRun {
        activity: "not_declared".to_owned(),
        after: NodeId(0),
    };
    assert_eq!(
        regenerate_gleam(&package, &graph, &delta),
        Err(StructureError::UnknownActivity {
            activity: "not_declared".to_owned(),
        })
    );
    Ok(())
}

#[test]
fn delta_targeting_absent_node_is_refused() -> TestResult {
    let package = saga_package()?;
    let graph = run_only_graph();
    let delta = StructuralDelta::RemoveNode { id: NodeId(99) };
    assert_eq!(
        regenerate_gleam(&package, &graph, &delta),
        Err(StructureError::DeltaTargetMissing { id: 99 })
    );
    Ok(())
}

#[test]
fn regeneration_does_not_mutate_the_package_or_graph() -> TestResult {
    let package = saga_package()?;
    let graph = run_only_graph();

    let source_before = package.source().clone();
    let graph_before = graph.clone();

    let delta = StructuralDelta::AppendRun {
        activity: "charge_payment".to_owned(),
        after: NodeId(0),
    };
    let _ = regenerate_gleam(&package, &graph, &delta)?;

    assert_eq!(package.source(), &source_before, "source map is untouched");
    assert_eq!(graph, graph_before, "graph is untouched");
    Ok(())
}

/// C24: a bounded delta over a Run-only sub-shape regenerates Gleam that
/// type-checks under `gleam build`. Gated at runtime on the `gleam` binary;
/// skips with a logged line when it is absent, never `#[ignore]`.
#[test]
fn append_run_regenerates_type_checking_gleam() -> TestResult {
    let package = saga_package()?;
    let graph = run_only_graph();

    let delta = StructuralDelta::AppendRun {
        activity: "charge_payment".to_owned(),
        after: NodeId(2),
    };
    let regenerated = regenerate_gleam(&package, &graph, &delta)?;

    if Command::new("gleam").arg("--version").output().is_err() {
        tracing::info!(
            "skipping append_run_regenerates_type_checking_gleam: `gleam` binary not available"
        );
        return Ok(());
    }

    assert_gleam_type_checks(&regenerated)?;
    Ok(())
}

/// Builds a throwaway gleam project that depends on the local `aion_flow`,
/// writes `regenerated` as its workflow module, and runs `gleam build`,
/// asserting success.
fn assert_gleam_type_checks(regenerated: &str) -> TestResult {
    let temp = tempfile::Builder::new()
        .prefix("aion-structure-regen-")
        .tempdir()?;
    let project = temp.path();
    std::fs::create_dir_all(project.join("src"))?;

    let aion_flow = workspace_root()
        .join("gleam")
        .join("aion_flow")
        .canonicalize()?;
    let gleam_toml = format!(
        "name = \"regen_check\"\n\
         version = \"0.1.0\"\n\
         target = \"erlang\"\n\n\
         [dependencies]\n\
         aion_flow = {{ path = \"{}\" }}\n\
         gleam_stdlib = \">= 0.34.0 and < 2.0.0\"\n\
         gleam_json = \">= 2.0.0 and < 4.0.0\"\n",
        aion_flow.display()
    );
    std::fs::write(project.join("gleam.toml"), gleam_toml)?;
    std::fs::write(project.join("src").join("regen_check.gleam"), regenerated)?;

    let output = Command::new("gleam")
        .arg("build")
        .current_dir(project)
        .output()?;

    assert!(
        output.status.success(),
        "regenerated Gleam must type-check; gleam build failed:\nstdout:\n{}\nstderr:\n{}\n--- module ---\n{regenerated}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    Ok(())
}
