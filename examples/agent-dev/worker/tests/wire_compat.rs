//! Wire compatibility against the Gleam codecs — the load-bearing test.
//!
//! The `agent_dev` package's emitted schemas (`../schemas/`) are the wire
//! contract and these serde types conform to them. Every literal below is
//! hand-derived from the codec source in `../src/agent_dev_codecs.gleam`
//! (each case names the codec function it mirrors). Both sides emit compact
//! JSON in field-declaration order, so each value must round-trip to the
//! literal **byte for byte**: any field-name, nesting, or field-order drift
//! fails here instead of at the first live dispatch.
//!
//! This test exists because the first integration audit found exactly this
//! class of break twice (a missing `run_id`, a flat-vs-nested `land` input)
//! and one asymmetry that only "worked" through serde's unknown-field
//! tolerance. Contracts are pinned by tests, not by luck.

use std::error::Error;
use std::fmt::Debug;

use agent_dev_worker::types::{
    GateInput, GateResult, LandInput, LandResult, ProvisionInput, Workspace,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

type TestResult = Result<(), Box<dyn Error>>;

/// Decode the literal and require equality, then encode the value and
/// require the exact literal back.
fn assert_wire<T>(literal: &str, expected: &T) -> TestResult
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let decoded: T = serde_json::from_str(literal)
        .map_err(|error| format!("failed to decode {literal}: {error}"))?;
    assert_eq!(&decoded, expected, "decode mismatch for {literal}");
    let encoded = serde_json::to_string(expected)?;
    assert_eq!(encoded, literal, "encode drift from the Gleam codec shape");
    Ok(())
}

/// Shape from `provision_input_to_json`: `repo_url`, `base_ref`, `brief_id`, `run_id`.
#[test]
fn provision_input_matches_the_gleam_codec() -> TestResult {
    assert_wire(
        r#"{"repo_url":"/Users/tom/Developer/ablative/chiron","base_ref":"main","brief_id":"CHIRON-RUFF-001","run_id":"0b81e178-56ff-4b2c-9f3f-2c1f42fd3a11"}"#,
        &ProvisionInput {
            repo_url: "/Users/tom/Developer/ablative/chiron".to_owned(),
            base_ref: "main".to_owned(),
            brief_id: "CHIRON-RUFF-001".to_owned(),
            run_id: "0b81e178-56ff-4b2c-9f3f-2c1f42fd3a11".to_owned(),
        },
    )
}

/// Shape from `workspace_to_json`: `path`, `branch`. This is BOTH the provision
/// output and — passed through whole — the `gate` activity input.
#[test]
fn workspace_and_gate_input_match_the_gleam_codec() -> TestResult {
    let literal = r#"{"path":"/Users/tom/.aion/clones/0b81e178-56ff-4b2c-9f3f-2c1f42fd3a11/repo","branch":"agent-dev-CHIRON-RUFF-001"}"#;
    let workspace = Workspace {
        path: "/Users/tom/.aion/clones/0b81e178-56ff-4b2c-9f3f-2c1f42fd3a11/repo".to_owned(),
        branch: "agent-dev-CHIRON-RUFF-001".to_owned(),
    };
    assert_wire(literal, &workspace)?;
    // GateInput IS the whole Workspace shape — no projection, no reliance on
    // serde's unknown-field tolerance.
    let gate_input: GateInput = serde_json::from_str(literal)?;
    assert_eq!(gate_input, workspace);
    Ok(())
}

/// Shape from `gate_detail_to_json`: `pass`, `diagnostics`.
#[test]
fn gate_result_matches_the_gleam_codec() -> TestResult {
    assert_wire(
        r#"{"pass":false,"diagnostics":"cargo clippy failed — exit status 101:\nerror: unused variable"}"#,
        &GateResult {
            pass: false,
            diagnostics: "cargo clippy failed — exit status 101:\nerror: unused variable"
                .to_owned(),
        },
    )
}

/// Shape from `land_input_to_json`: the workspace NESTED (the workflow passes
/// the provision output record through whole), then `brief_id`.
#[test]
fn land_input_matches_the_gleam_codec() -> TestResult {
    assert_wire(
        r#"{"workspace":{"path":"/Users/tom/.aion/clones/0b81e178-56ff-4b2c-9f3f-2c1f42fd3a11/repo","branch":"agent-dev-CHIRON-RUFF-001"},"brief_id":"CHIRON-RUFF-001"}"#,
        &LandInput {
            workspace: Workspace {
                path: "/Users/tom/.aion/clones/0b81e178-56ff-4b2c-9f3f-2c1f42fd3a11/repo"
                    .to_owned(),
                branch: "agent-dev-CHIRON-RUFF-001".to_owned(),
            },
            brief_id: "CHIRON-RUFF-001".to_owned(),
        },
    )
}

/// Shape from `land_output_to_json`: `commit_sha`.
#[test]
fn land_result_matches_the_gleam_codec() -> TestResult {
    assert_wire(
        r#"{"commit_sha":"4f2a1c9d8e7b6a5f4e3d2c1b0a9f8e7d6c5b4a39"}"#,
        &LandResult {
            commit_sha: "4f2a1c9d8e7b6a5f4e3d2c1b0a9f8e7d6c5b4a39".to_owned(),
        },
    )
}

/// The emitted schema files this module conforms to exist beside the package
/// — a rename or move of the contract directory should fail loudly here.
#[test]
fn contract_schemas_exist_beside_the_package() {
    for schema in [
        "provision_input.json",
        "workspace.json",
        "gate_detail.json",
        "land_input.json",
        "land_output.json",
    ] {
        let path = format!("{}/../schemas/{schema}", env!("CARGO_MANIFEST_DIR"));
        assert!(
            std::path::Path::new(&path).is_file(),
            "contract schema missing: {path}"
        );
    }
}
