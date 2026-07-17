//! Workflow process helpers exposed by `RuntimeHandle`.

use aion_core::{Payload, WorkflowError};

use crate::{EngineError, Pid, RuntimeHandle};

impl RuntimeHandle {
    /// Block until a workflow exits and convert its terminal runtime outcome.
    ///
    /// Normal returns become durable result payloads. Abnormal exits become typed
    /// workflow errors so lifecycle code can record a terminal failure.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the result term cannot be converted
    /// into a payload.
    pub fn workflow_outcome(
        &self,
        pid: Pid,
    ) -> Result<Result<Payload, WorkflowError>, EngineError> {
        let owned = self.process_exits.get(pid)?.wait()?;
        let outcome = super::outcome::workflow_outcome(&self.atom_table, pid, &owned);
        self.release_spawn_heaps(pid);
        outcome
    }
}
