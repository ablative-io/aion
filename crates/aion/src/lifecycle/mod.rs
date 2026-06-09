//! Workflow lifecycle operations: start, terminate, suspend, and resume.

/// Workflow completion notification helpers.
pub mod completion;
/// Continue-as-new lifecycle transition support.
pub mod continue_as_new;
/// Workflow start request and initialization helpers.
pub mod start;
/// Terminal workflow transitions: complete, fail, and cancel.
pub mod terminate;
/// Runtime suspend and resume transition helpers.
pub mod transition;
/// Visibility projection helpers for lifecycle events.
pub mod visibility;

pub use continue_as_new::{ContinueAsNewContext, ContinueAsNewRequest, continue_as_new};
pub use terminate::{TerminateWorkflowContext, cancel, complete, fail};
pub use transition::{resume, suspend};
