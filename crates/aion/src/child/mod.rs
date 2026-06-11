//! Child-workflow spawning helpers.

/// Record-then-spawn child-workflow primitives over the AE engine seam.
pub mod spawn;

pub use spawn::{ChildWorkflowError, ChildWorkflowRecordingContext, SpawnedChildWorkflow, spawn};
