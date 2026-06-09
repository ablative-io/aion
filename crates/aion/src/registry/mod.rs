//! Workflow registry handles and tables.

/// Workflow handle residency, completion, and mailbox state.
pub mod handle;
/// In-memory workflow registry table.
pub mod table;

pub use handle::{
    CompletionNotifier, HandleResidency, Residency, TerminalOutcome, WorkflowHandle,
    WorkflowHandleParts,
};
pub use table::Registry;
