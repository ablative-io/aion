//! Describe-workflow response projection.
//!
//! The ops console's `POST /workflows/describe` read consumes exactly this shape:
//! a workflow [`WorkflowSummary`] projection plus the run's event [`Event`]
//! history as plain JSON. Defining it here lets the same type be exported to
//! TypeScript (so the generated bindings match the wire by construction) and be
//! produced directly by the HTTP handler at the transport boundary.

use serde::{Deserialize, Serialize};

use crate::{Event, WorkflowSummary};

/// Response to a describe-workflow request.
///
/// `history` is the run's events as plain serialized [`Event`] values (never a
/// protobuf-derived envelope), so the ops console decodes each entry directly.
/// When `include_history` is false the server returns an empty `history`.
#[derive(Serialize, Deserialize, ts_rs::TS, Clone, Debug, PartialEq)]
pub struct DescribeWorkflowResponse {
    /// Workflow summary projected from authoritative history, when the workflow
    /// exists.
    pub summary: Option<WorkflowSummary>,
    /// The run's event history as plain serialized events.
    pub history: Vec<Event>,
}
