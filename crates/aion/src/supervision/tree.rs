//! Structural supervision tree for engine, workflow-type, workflow, and activity nodes.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, MutexGuard};

use crate::{EngineError, Pid};

/// Stable identifier for the single engine supervisor root.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EngineSupervisorId;

/// Stable identifier for a per-workflow-type supervisor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeSupervisorId {
    workflow_type: String,
}

impl TypeSupervisorId {
    /// Logical workflow type supervised by this node.
    #[must_use]
    pub fn workflow_type(&self) -> &str {
        &self.workflow_type
    }
}

/// Snapshot of a per-type supervisor and the workflow processes under it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeSupervisorNode {
    id: TypeSupervisorId,
    workflow_processes: BTreeSet<Pid>,
}

impl TypeSupervisorNode {
    /// Identifier for this per-type supervisor.
    #[must_use]
    pub fn id(&self) -> &TypeSupervisorId {
        &self.id
    }

    /// Workflow process PIDs directly supervised by this type supervisor.
    #[must_use]
    pub fn workflow_processes(&self) -> &BTreeSet<Pid> {
        &self.workflow_processes
    }
}

/// Snapshot of a workflow process and its linked activity children.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowNode {
    workflow_type: String,
    workflow_pid: Pid,
    activity_children: BTreeSet<Pid>,
}

impl WorkflowNode {
    /// Logical workflow type for this process.
    #[must_use]
    pub fn workflow_type(&self) -> &str {
        &self.workflow_type
    }

    /// Workflow process PID.
    #[must_use]
    pub fn workflow_pid(&self) -> Pid {
        self.workflow_pid
    }

    /// Activity child PIDs one level below this workflow process.
    #[must_use]
    pub fn activity_children(&self) -> &BTreeSet<Pid> {
        &self.activity_children
    }

    /// Returns true when the activity is recorded as a linked child of this workflow.
    #[must_use]
    pub fn has_activity_child(&self, activity_pid: Pid) -> bool {
        self.activity_children.contains(&activity_pid)
    }
}

#[derive(Debug, Default)]
struct TreeState {
    type_supervisors: HashMap<String, TypeSupervisorNode>,
    workflows: HashMap<Pid, WorkflowNode>,
}

/// In-memory model of Aion's three-level supervision structure.
///
/// The real cancellation and crash behavior is provided by beamr links through
/// the runtime boundary. This tree records Aion's intended parent-child shape:
/// one engine supervisor, one supervisor per distinct workflow type, workflow
/// processes under their type supervisor, and linked activity children directly
/// under each workflow process.
#[derive(Debug, Default)]
pub struct SupervisionTree {
    state: Mutex<TreeState>,
}

impl SupervisionTree {
    /// Create an empty supervision tree with the single engine root present.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Identifier for the single engine supervisor root.
    #[must_use]
    pub const fn engine_supervisor(&self) -> EngineSupervisorId {
        EngineSupervisorId
    }

    /// Ensure there is exactly one supervisor for `workflow_type`.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] if the tree lock was poisoned.
    pub fn ensure_type_supervisor(
        &self,
        workflow_type: impl Into<String>,
    ) -> Result<TypeSupervisorId, EngineError> {
        let workflow_type = workflow_type.into();
        let mut state = self.state()?;
        let supervisor = state
            .type_supervisors
            .entry(workflow_type.clone())
            .or_insert_with(|| TypeSupervisorNode {
                id: TypeSupervisorId { workflow_type },
                workflow_processes: BTreeSet::new(),
            });
        Ok(supervisor.id.clone())
    }

    /// Place a workflow process under its per-type supervisor.
    ///
    /// This creates the per-type supervisor on first use and never creates a
    /// supervisor per workflow execution.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] if the tree lock was poisoned.
    pub fn place_workflow(
        &self,
        workflow_type: impl Into<String>,
        workflow_pid: Pid,
    ) -> Result<TypeSupervisorId, EngineError> {
        let workflow_type = workflow_type.into();
        let mut state = self.state()?;
        let supervisor = state
            .type_supervisors
            .entry(workflow_type.clone())
            .or_insert_with(|| TypeSupervisorNode {
                id: TypeSupervisorId {
                    workflow_type: workflow_type.clone(),
                },
                workflow_processes: BTreeSet::new(),
            });
        supervisor.workflow_processes.insert(workflow_pid);
        let id = supervisor.id.clone();
        state.workflows.insert(
            workflow_pid,
            WorkflowNode {
                workflow_type,
                workflow_pid,
                activity_children: BTreeSet::new(),
            },
        );
        Ok(id)
    }

    /// Record an already-spawned linked activity child under its workflow process.
    ///
    /// Call this only after [`crate::RuntimeHandle::spawn_activity`] succeeds;
    /// that runtime call is the authoritative BEAM link establishment.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] if the tree lock was poisoned, or
    /// [`EngineError::Runtime`] when `workflow_pid` is not present in the tree.
    pub fn record_activity_child(
        &self,
        workflow_pid: Pid,
        activity_pid: Pid,
    ) -> Result<(), EngineError> {
        let mut state = self.state()?;
        let workflow =
            state
                .workflows
                .get_mut(&workflow_pid)
                .ok_or_else(|| EngineError::Runtime {
                    reason: format!(
                        "workflow process {workflow_pid} is not in the supervision tree"
                    ),
                })?;
        workflow.activity_children.insert(activity_pid);
        Ok(())
    }

    /// Number of per-workflow-type supervisors under the engine root.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] if the tree lock was poisoned.
    pub fn type_supervisor_count(&self) -> Result<usize, EngineError> {
        Ok(self.state()?.type_supervisors.len())
    }

    /// Snapshot all per-type supervisors without holding the tree lock.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] if the tree lock was poisoned.
    pub fn type_supervisors(&self) -> Result<Vec<TypeSupervisorNode>, EngineError> {
        Ok(self.state()?.type_supervisors.values().cloned().collect())
    }

    /// Snapshot a workflow node by PID.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::RegistryPoisoned`] if the tree lock was poisoned.
    pub fn workflow(&self, workflow_pid: Pid) -> Result<Option<WorkflowNode>, EngineError> {
        Ok(self.state()?.workflows.get(&workflow_pid).cloned())
    }

    fn state(&self) -> Result<MutexGuard<'_, TreeState>, EngineError> {
        self.state.lock().map_err(|_| EngineError::RegistryPoisoned)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::EngineError;

    use super::SupervisionTree;

    #[test]
    fn one_engine_root_has_one_supervisor_per_workflow_type() -> Result<(), EngineError> {
        let tree = SupervisionTree::new();
        let root = tree.engine_supervisor();
        let checkout = tree.ensure_type_supervisor("checkout")?;
        let billing = tree.ensure_type_supervisor("billing")?;
        let checkout_again = tree.ensure_type_supervisor("checkout")?;

        assert_eq!(root, tree.engine_supervisor());
        assert_eq!(checkout.workflow_type(), "checkout");
        assert_eq!(billing.workflow_type(), "billing");
        assert_eq!(checkout, checkout_again);
        assert_eq!(tree.type_supervisor_count()?, 2);
        Ok(())
    }

    #[test]
    fn workflows_sit_under_type_supervisors_not_new_supervisors() -> Result<(), EngineError> {
        let tree = SupervisionTree::new();

        tree.place_workflow("checkout", 10)?;
        tree.place_workflow("checkout", 11)?;

        assert_eq!(tree.type_supervisor_count()?, 1);
        let supervisors = tree.type_supervisors()?;
        let checkout = supervisors
            .iter()
            .find(|node| node.id().workflow_type() == "checkout");
        assert!(checkout.is_some());
        if let Some(checkout) = checkout {
            assert_eq!(checkout.workflow_processes().len(), 2);
            assert!(checkout.workflow_processes().contains(&10));
            assert!(checkout.workflow_processes().contains(&11));
        }
        Ok(())
    }

    #[test]
    fn activity_children_are_one_level_below_workflow_process() -> Result<(), EngineError> {
        let tree = SupervisionTree::new();

        tree.place_workflow("checkout", 10)?;
        tree.record_activity_child(10, 20)?;
        tree.record_activity_child(10, 21)?;

        let workflow = tree.workflow(10)?;
        assert!(workflow.is_some());
        if let Some(workflow) = workflow {
            assert_eq!(workflow.workflow_type(), "checkout");
            assert_eq!(workflow.workflow_pid(), 10);
            assert!(workflow.has_activity_child(20));
            assert!(workflow.has_activity_child(21));
            assert_eq!(workflow.activity_children().len(), 2);
        }
        Ok(())
    }

    #[test]
    fn missing_workflow_activity_parent_is_typed_error() {
        let tree = SupervisionTree::new();

        let error = tree.record_activity_child(99, 20);

        assert!(matches!(error, Err(EngineError::Runtime { .. })));
    }

    #[test]
    fn poisoned_tree_lock_returns_typed_registry_error() {
        let tree = Arc::new(SupervisionTree::new());
        let poisoner_tree = Arc::clone(&tree);
        let poisoner = std::thread::spawn(move || {
            let guard = poisoner_tree.state.lock();
            assert!(guard.is_ok());
            std::panic::resume_unwind(Box::new("poison supervision tree lock"));
        });

        assert!(poisoner.join().is_err());
        assert!(matches!(
            tree.type_supervisor_count(),
            Err(EngineError::RegistryPoisoned)
        ));
    }
}
