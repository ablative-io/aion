//! Wire types for the `agent_dev` workflow contract.
//!
//! Defined field-for-field from the shared contract (the `agent_dev` GLEAM
//! package is authored separately; wire-compat pinning against its real
//! codecs happens at integration). Deserialization is tolerant of extra
//! fields, matching the Gleam field-decoder convention.
//!
//! The three AGENT activity types (`scout`, `dev`, `review`) do not appear
//! here: their input is a plain JSON `String` (the prompt) and their output a
//! plain JSON `String` (the agent's answer), carried by the composed harness
//! path rather than a typed registry handler.

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

/// Input of the `gate` activity: the workspace to check.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GateInput {
    /// Absolute path of the workspace clone the gate runs in.
    pub path: String,
}

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
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LandInput {
    /// Absolute path of the workspace clone to commit in.
    pub path: String,
    /// The brief this run implements; names the commit.
    pub brief_id: String,
}

/// Output of the `land` activity: the created commit.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LandResult {
    /// The full SHA of the commit `land` created.
    pub commit_sha: String,
}
