//! map: dynamic fan-out from a runtime list
//!
//! `map` turns a runtime list into child workflow specs in input order, then delegates to the
//! [`all`](crate::concurrency::all) collector for linked spawning, ordered result collection, and
//! fail-fast cancellation. Empty input produces no children and returns an empty result.

use aion_core::Payload;

use crate::concurrency::CorrelationMailbox;
use crate::concurrency::all::{AllChildWorkflowSpec, AllError, AllRecordingContext, all};
use crate::engine_seam::EngineHandle;

/// Applies `f` to each input item and returns child specs in the same order as `items`.
#[must_use]
pub fn child_specs_from_items<T, F>(items: &[T], f: F) -> Vec<AllChildWorkflowSpec>
where
    F: FnMut(&T) -> AllChildWorkflowSpec,
{
    items.iter().map(f).collect()
}

/// Dynamically fans out child workflows from runtime `items` and collects results like [`all`].
///
/// The spec-producing function is called exactly once for each item, in input order. The produced
/// specs are then passed to [`all`], so results are ordered by input element position and any child
/// failure fails fast while cancelling remaining linked children. Empty input produces zero child
/// spawns and returns an empty result.
///
/// # Errors
///
/// Returns [`AllError`] from the delegated [`all`] collector when spawning, matching,
/// cancellation, or a child outcome fails.
pub fn map<T, F>(
    engine: &impl EngineHandle,
    recording: &AllRecordingContext,
    mailbox: &mut impl CorrelationMailbox,
    items: &[T],
    f: F,
) -> Result<Vec<Payload>, AllError>
where
    F: FnMut(&T) -> AllChildWorkflowSpec,
{
    let specs = child_specs_from_items(items, f);
    all(engine, recording, mailbox, &specs)
}

#[cfg(test)]
mod tests {
    use aion_core::{ContentType, Event, Payload, RunId, WorkflowError, WorkflowId};
    use chrono::DateTime;

    use super::{child_specs_from_items, map};
    use crate::concurrency::VecCorrelationMailbox;
    use crate::concurrency::all::{AllChildWorkflowSpec, AllError, AllRecordingContext};
    use crate::engine_seam::test_support::FakeEngineHandle;
    use crate::engine_seam::{
        ChildWorkflowSpawnResult, WorkflowMailboxMessage, WorkflowProcessHandle,
    };

    fn payload(bytes: &'static [u8]) -> Payload {
        Payload::new(ContentType::Json, bytes.to_vec())
    }

    fn timestamp() -> Result<DateTime<chrono::Utc>, Box<dyn std::error::Error>> {
        Ok(DateTime::parse_from_rfc3339("2026-06-04T12:00:00Z").map(DateTime::from)?)
    }

    fn workflow_error(message: &str) -> WorkflowError {
        WorkflowError {
            message: message.to_owned(),
            details: None,
        }
    }

    fn spec_from_item(item: u8) -> AllChildWorkflowSpec {
        AllChildWorkflowSpec::new(
            "child",
            Payload::new(ContentType::Json, vec![item]),
            RunId::new_v4(),
        )
    }

    fn queue_spawns(
        engine: &FakeEngineHandle,
        children: &[WorkflowId],
    ) -> Result<Vec<WorkflowProcessHandle>, Box<dyn std::error::Error>> {
        let mut processes = Vec::with_capacity(children.len());
        for (index, child) in children.iter().enumerate() {
            let pid = u64::try_from(index)?.saturating_add(10);
            let process = WorkflowProcessHandle::new(pid);
            processes.push(process);
            engine.push_child_spawn_response(Ok(ChildWorkflowSpawnResult {
                child_workflow_id: child.clone(),
                child_process: process,
            }))?;
        }
        Ok(processes)
    }

    #[test]
    fn child_specs_from_items_preserves_runtime_item_order() {
        let items = [4_u8, 3, 2, 1];

        let specs = child_specs_from_items(&items, |&item| spec_from_item(item));

        assert_eq!(specs.len(), 4);
        let inputs: Vec<Payload> = specs.into_iter().map(|spec| spec.input).collect();
        assert_eq!(
            inputs,
            vec![payload(&[4]), payload(&[3]), payload(&[2]), payload(&[1])]
        );
    }

    #[test]
    fn map_empty_input_produces_no_children_and_empty_result()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let recording = AllRecordingContext::new(WorkflowId::new_v4(), 10, timestamp()?);
        let mut mailbox = VecCorrelationMailbox::new(Vec::new());
        let items: [u8; 0] = [];

        let results = map(&engine, &recording, &mut mailbox, &items, |&item| {
            spec_from_item(item)
        })?;

        assert_eq!(results, Vec::<Payload>::new());
        assert!(engine.child_spawn_requests()?.is_empty());
        assert!(engine.recorded_events()?.is_empty());
        assert!(mailbox.is_empty());
        Ok(())
    }

    #[test]
    fn map_collects_out_of_order_child_results_in_input_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        let children = vec![
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
        ];
        queue_spawns(&engine, &children)?;
        let items = [0_u8, 1, 2];
        let recording = AllRecordingContext::new(parent.clone(), 40, timestamp()?);
        let result_a = payload(br#"{"result":0}"#);
        let result_b = payload(br#"{"result":1}"#);
        let result_c = payload(br#"{"result":2}"#);
        let mut mailbox = VecCorrelationMailbox::new(vec![
            WorkflowMailboxMessage::ChildWorkflowCompleted {
                child_workflow_id: children[2].clone(),
                correlation: 42,
                result: result_c.clone(),
            },
            WorkflowMailboxMessage::ChildWorkflowCompleted {
                child_workflow_id: children[0].clone(),
                correlation: 40,
                result: result_a.clone(),
            },
            WorkflowMailboxMessage::ChildWorkflowCompleted {
                child_workflow_id: children[1].clone(),
                correlation: 41,
                result: result_b.clone(),
            },
        ]);

        let results = map(&engine, &recording, &mut mailbox, &items, |&item| {
            spec_from_item(item)
        })?;

        assert_eq!(results, vec![result_a, result_b, result_c]);
        assert!(mailbox.is_empty());
        let requests = engine.child_spawn_requests()?;
        assert_eq!(requests.len(), 3);
        let spawned_inputs: Vec<Payload> =
            requests.into_iter().map(|request| request.input).collect();
        assert_eq!(
            spawned_inputs,
            vec![payload(&[0]), payload(&[1]), payload(&[2])]
        );
        let recorded = engine.recorded_events()?;
        assert_eq!(recorded.len(), 3);
        assert!(
            recorded
                .iter()
                .all(|(workflow_id, _)| workflow_id == &parent)
        );
        assert!(
            recorded
                .iter()
                .all(|(_, event)| matches!(event, Event::ChildWorkflowStarted { .. }))
        );
        Ok(())
    }

    #[test]
    fn map_fails_fast_and_cancels_remaining_children() -> Result<(), Box<dyn std::error::Error>> {
        let engine = FakeEngineHandle::new();
        let parent = WorkflowId::new_v4();
        let children = vec![
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
            WorkflowId::new_v4(),
        ];
        let processes = queue_spawns(&engine, &children)?;
        let items = [0_u8, 1, 2];
        let recording = AllRecordingContext::new(parent.clone(), 50, timestamp()?);
        let failure = workflow_error("boom");
        let mut mailbox =
            VecCorrelationMailbox::new(vec![WorkflowMailboxMessage::ChildWorkflowFailed {
                child_workflow_id: children[1].clone(),
                correlation: 51,
                error: failure.clone(),
            }]);

        let error = map(&engine, &recording, &mut mailbox, &items, |&item| {
            spec_from_item(item)
        });

        assert_eq!(
            error,
            Err(AllError::ChildFailed {
                child_workflow_id: children[1].clone(),
                error: failure,
            })
        );
        assert!(mailbox.is_empty());
        assert_eq!(
            engine.terminated_child_workflows()?,
            vec![
                (parent.clone(), processes[0], 50),
                (parent.clone(), processes[2], 52),
            ]
        );
        let recorded = engine.recorded_events()?;
        assert_eq!(recorded.len(), 5);
        assert!(matches!(
            recorded[3].1,
            Event::ChildWorkflowCancelled { .. }
        ));
        assert_eq!(recorded[3].1.seq(), 54);
        assert!(matches!(
            recorded[4].1,
            Event::ChildWorkflowCancelled { .. }
        ));
        assert_eq!(recorded[4].1.seq(), 55);
        Ok(())
    }
}
