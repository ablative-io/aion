//! Wire types for the `agent_dev` and `assistant` workflow contracts.
//!
//! THE RULE: the Gleam packages are the contract source, and their emitted
//! schemas (`examples/agent-dev/schemas/`, `examples/assistant/schemas/`)
//! are the wire contract — this module conforms to them, never the other way
//! round. The `wire_compat` pinning test holds each type to its Gleam codec
//! shape byte for byte. Deserialization is tolerant of extra fields,
//! matching the Gleam field-decoder convention.
//!
//! The AGENT activity types (`scout`, `dev`, `review`, `assistant`) do not
//! appear here: their input is a plain JSON `String` (the prompt) and their
//! output a plain JSON `String` (the agent's answer), carried by the
//! composed harness path rather than a typed registry handler.

use serde::{Deserialize, Serialize};

/// Input of the `provision` activity: clone `repo_url` into the per-run
/// workspace and create the working branch off `base_ref`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProvisionInput {
    /// The URL (or local path) `git clone` fetches from.
    pub repo_url: String,
    /// The ref the working branch is created from.
    pub base_ref: String,
    /// The brief this run implements; names the working branch.
    pub brief_id: String,
    /// The workflow id string, passed IN THE ACTIVITY INPUT by the workflow.
    /// Keys the per-run workspace directory `<root>/<run_id>/repo`.
    pub run_id: String,
}

/// Output of the `provision` activity: the provisioned clone.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Workspace {
    /// Absolute path of the cloned repository (`<root>/<run_id>/repo`).
    pub path: String,
    /// The working branch (`agent-dev-<brief_id>`), checked out in the clone.
    pub branch: String,
}

/// Input of the `assistant_provision` activity (the `assistant` workflow
/// package): materialise the session workspace at `<root>/<run_id>/repo` — a
/// clone of `repo_path` when given, a fresh scratch git workspace when empty.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AssistantProvisionInput {
    /// The repository the session grounds its answers in: a local path or
    /// clone URL. Empty means no repository — provision a scratch workspace.
    pub repo_path: String,
    /// The workflow id string, passed IN THE ACTIVITY INPUT by the workflow.
    /// Keys the per-run workspace directory `<root>/<run_id>/repo`.
    pub run_id: String,
}

/// Output of the `assistant_provision` activity: the provisioned session
/// workspace.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AssistantWorkspace {
    /// Absolute path of the session workspace (`<root>/<run_id>/repo`).
    pub path: String,
}

/// Input of the `gate` activity: the workspace to check. Per the contract,
/// the workflow passes the `provision` output record through WHOLE — this is
/// the full [`Workspace`] shape, not a projection, so the wire matches the
/// Gleam `workspace_to_json` codec byte for byte (no reliance on serde's
/// unknown-field tolerance).
pub type GateInput = Workspace;

/// Output of the `gate` activity: a recorded verdict, never an error — a
/// gate that RAN and failed is data the workflow routes on.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GateResult {
    /// Whether both gate commands exited zero.
    pub pass: bool,
    /// The failing command's combined-output tail; empty on a pass.
    pub diagnostics: String,
}

/// Input of the `land` activity: commit the run's work in the workspace.
/// Per the contract, the workspace arrives NESTED — the workflow passes the
/// `provision` output record through whole.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LandInput {
    /// The provisioned workspace clone to commit in.
    pub workspace: Workspace,
    /// The brief this run implements; names the commit.
    pub brief_id: String,
}

/// Output of the `land` activity: the created commit.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LandResult {
    /// The full SHA of the commit `land` created.
    pub commit_sha: String,
}
