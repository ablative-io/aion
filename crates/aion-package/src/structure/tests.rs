//! Tests for the workflow structure projection.
//!
//! The C23 diff test loads the real `examples/order-saga` workflow source,
//! extracts its structure, and asserts the node/edge set matches the saga's
//! known shape — proving the graph is derived from the source, not hand-drawn.
//! Vocabulary-coverage tests drive each primitive from inline fixtures, and the
//! C24 round-trip test regenerates Gleam and type-checks it with the real
//! `gleam` binary, gated at runtime (it skips with a logged line when `gleam`
//! is absent, never `#[ignore]`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion,
    Package, content_hash,
};

use super::error::StructureError;
use super::extract::extract_structure;
use super::model::{CorrelationKey, EdgeKind, NodeId, NodePrimitive, WorkflowGraph};
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

/// Builds a loadable package whose entry module carries `source` and whose
/// manifest declares `activities`. Extraction reads the source map, so the beam
/// for the entry module is present (load validation requires it) but its bytes
/// are otherwise irrelevant.
fn package_with(entry_module: &str, source: &str, activities: &[&str]) -> Result<Package, String> {
    let beams = BeamSet::new(vec![BeamModule::new(entry_module, vec![1, 2, 3])])
        .map_err(|error| error.to_string())?;
    let hash = content_hash(&beams);
    let manifest = Manifest {
        entry_module: entry_module.to_owned(),
        entry_function: "run".to_owned(),
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

// --- R1 / C21 / C23: structure extraction and the node/edge diff ------------

#[test]
fn order_saga_extracts_to_six_sequential_run_nodes() -> TestResult {
    let source = order_saga_source()?;
    let package = package_with("order_saga", &source, SAGA_ACTIVITIES)?;

    let graph = extract_structure(&package)?;

    assert_eq!(graph.entry_module(), "order_saga");
    assert_eq!(graph.nodes().len(), SAGA_ACTIVITIES.len(), "six run nodes");

    for (ordinal, expected) in SAGA_ACTIVITIES.iter().enumerate() {
        let node = &graph.nodes()[ordinal];
        assert_eq!(node.id, NodeId(ordinal));
        assert_eq!(node.primitive, NodePrimitive::Run);
        assert_eq!(
            node.correlation,
            CorrelationKey::ActivitySequence {
                ordinal,
                activity: (*expected).to_owned(),
            },
            "node {ordinal} correlates to {expected} at its call-order ordinal"
        );
    }

    // Sequential edges chain every node to its successor, in order.
    let expected_edges: Vec<_> = (0..SAGA_ACTIVITIES.len() - 1)
        .map(|index| (NodeId(index), NodeId(index + 1)))
        .collect();
    let actual_edges: Vec<_> = graph
        .edges()
        .iter()
        .map(|edge| {
            assert_eq!(edge.kind, EdgeKind::Sequence);
            (edge.from, edge.to)
        })
        .collect();
    assert_eq!(actual_edges, expected_edges);

    Ok(())
}

/// The diff primitive proves the extracted set matches a known structure
/// regardless of internal ordering — and detects a mismatch.
#[test]
fn structure_diff_matches_known_set_and_detects_drift() -> TestResult {
    let source = order_saga_source()?;
    let package = package_with("order_saga", &source, SAGA_ACTIVITIES)?;
    let graph = extract_structure(&package)?;

    let known = known_saga_graph();
    assert!(
        graph.structurally_equals(&known),
        "extracted graph must match the saga's known node/edge set"
    );

    // A graph missing the final node is detected as different.
    let mut truncated = known.clone();
    truncated.nodes.pop();
    truncated.edges.pop();
    assert!(!graph.structurally_equals(&truncated));
    Ok(())
}

/// The saga's known structure, written by hand as the oracle the extraction is
/// diffed against (C23).
fn known_saga_graph() -> WorkflowGraph {
    let nodes = SAGA_ACTIVITIES
        .iter()
        .enumerate()
        .map(|(ordinal, activity)| super::model::GraphNode {
            id: NodeId(ordinal),
            primitive: NodePrimitive::Run,
            correlation: CorrelationKey::ActivitySequence {
                ordinal,
                activity: (*activity).to_owned(),
            },
        })
        .collect();
    let edges = (0..SAGA_ACTIVITIES.len() - 1)
        .map(|index| super::model::GraphEdge {
            from: NodeId(index),
            to: NodeId(index + 1),
            kind: EdgeKind::Sequence,
        })
        .collect();
    WorkflowGraph {
        entry_module: "order_saga".to_owned(),
        nodes,
        edges,
    }
}

#[test]
fn extraction_is_deterministic() -> TestResult {
    let source = order_saga_source()?;
    let package = package_with("order_saga", &source, SAGA_ACTIVITIES)?;
    let first = extract_structure(&package)?;
    let second = extract_structure(&package)?;
    assert_eq!(first, second);
    Ok(())
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
    };
    // No source for the entry module.
    let package = Package::from_validated_parts_for_test(manifest, beams, BTreeMap::new(), hash);

    let result = extract_structure(&package);
    assert_eq!(
        result,
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
    let package = package_with("plain", source, &[])?;
    assert_eq!(
        extract_structure(&package),
        Err(StructureError::NoWorkflowImport {
            module: "plain".to_owned(),
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
    // Manifest declares no `ghost` activity.
    let package = package_with("demo", source, &["real"])?;
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
         \u{20}\u{20}let _ = workflow.all([wrappers.a_activity(input)])\n\
         \u{20}\u{20}let _ = workflow.race([wrappers.a_activity(input)])\n\
         \u{20}\u{20}let _ = workflow.map([input], wrappers.a_activity)\n\
         \u{20}\u{20}let _ = workflow.receive(signal_ref)\n\
         \u{20}\u{20}let _ = workflow.sleep(duration.seconds(1))\n\
         \u{20}\u{20}let _ = workflow.start_timer(\"deadline\", duration.seconds(5))\n\
         \u{20}\u{20}workflow.run(wrappers.a_activity(input))\n\
         }\n";
    let package = package_with("demo", source, &["a"])?;
    let graph = extract_structure(&package)?;

    let primitives: Vec<_> = graph.nodes().iter().map(|node| node.primitive).collect();
    assert_eq!(
        primitives,
        vec![
            NodePrimitive::All,
            NodePrimitive::Race,
            NodePrimitive::Map,
            NodePrimitive::Receive,
            NodePrimitive::Sleep,
            NodePrimitive::StartTimer,
            NodePrimitive::Run,
        ]
    );

    // receive carries the signal reference identifier.
    assert_eq!(
        graph.nodes()[3].correlation,
        CorrelationKey::Signal {
            reference: "signal_ref".to_owned(),
        }
    );
    // start_timer carries its literal name.
    assert_eq!(
        graph.nodes()[5].correlation,
        CorrelationKey::Timer {
            id: "deadline".to_owned(),
        }
    );
    // run carries the activity name at ordinal 0 (the only activity dispatch).
    assert_eq!(
        graph.nodes()[6].correlation,
        CorrelationKey::ActivitySequence {
            ordinal: 0,
            activity: "a".to_owned(),
        }
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
    let package = package_with("demo", source, &[])?;
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
    let package = package_with("demo", source, &["a"])?;
    let graph = extract_structure(&package)?;
    assert_eq!(graph.nodes().len(), 1);
    assert_eq!(graph.nodes()[0].primitive, NodePrimitive::Run);
    Ok(())
}

// --- R2 / C24: bounded regeneration -----------------------------------------

#[test]
fn append_run_unknown_activity_is_refused() -> TestResult {
    let source = order_saga_source()?;
    let package = package_with("order_saga", &source, SAGA_ACTIVITIES)?;
    let graph = extract_structure(&package)?;

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
    let source = order_saga_source()?;
    let package = package_with("order_saga", &source, SAGA_ACTIVITIES)?;
    let graph = extract_structure(&package)?;

    let delta = StructuralDelta::RemoveNode { id: NodeId(99) };
    assert_eq!(
        regenerate_gleam(&package, &graph, &delta),
        Err(StructureError::DeltaTargetMissing { id: 99 })
    );
    Ok(())
}

#[test]
fn regeneration_does_not_mutate_the_package_or_graph() -> TestResult {
    let source = order_saga_source()?;
    let package = package_with("order_saga", &source, SAGA_ACTIVITIES)?;
    let graph = extract_structure(&package)?;

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

/// C24: a bounded delta regenerates Gleam that type-checks under `gleam build`.
/// Gated at runtime on the `gleam` binary; skips with a logged line when it is
/// absent, never `#[ignore]`.
#[test]
fn append_run_regenerates_type_checking_gleam() -> TestResult {
    let source = order_saga_source()?;
    let package = package_with("order_saga", &source, SAGA_ACTIVITIES)?;
    let graph = extract_structure(&package)?;

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
/// asserting success. The project's `aion_flow` path points back at
/// `gleam/aion_flow` so the regenerated module type-checks against the real SDK.
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
