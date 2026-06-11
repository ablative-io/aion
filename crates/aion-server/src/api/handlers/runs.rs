//! Run-id resolution and terminal-status reads for the shared handlers.

use aion_core::{RunId, WorkflowId, WorkflowStatus, WorkflowSummary};
use aion_proto::WireError;

use super::error::workflow_not_found_error;
use crate::ServerError;

pub(super) async fn resolve_run_id(
    engine: &aion::Engine,
    workflow_id: &WorkflowId,
    id: Option<aion_proto::ProtoRunId>,
) -> Result<RunId, WireError> {
    if let Some(id) = id {
        return id.try_into();
    }

    let chain = engine
        .store()
        .read_run_chain(workflow_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    chain
        .last()
        .map(|summary| summary.run_id.clone())
        .ok_or_else(|| workflow_not_found_error(workflow_id))
}

pub(super) async fn terminal_status(
    engine: &aion::Engine,
    workflow_id: &WorkflowId,
) -> Result<Option<WorkflowStatus>, WireError> {
    let history = engine
        .store()
        .read_history(workflow_id)
        .await
        .map_err(|error| ServerError::from(error).to_wire_error())?;
    Ok(WorkflowSummary::from_history(&history)
        .map(|summary| summary.status)
        .filter(|status| status.is_terminal()))
}

#[cfg(test)]
mod tests {
    use aion_core::RunId;

    use super::super::test_support::{
        NAMESPACE, append_continued_chain, context, describe_request, workflow_id,
    };
    use super::resolve_run_id;
    use crate::{NamespaceOperation, WorkflowTarget};

    #[tokio::test]
    async fn omitted_run_id_resolves_latest_run_from_chain()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        let first = RunId::new(uuid::Uuid::from_u128(11));
        let latest = RunId::new(uuid::Uuid::from_u128(12));
        context.ownership.record(workflow_id(), NAMESPACE)?;
        append_continued_chain(context.store.as_ref(), &first, &latest).await?;

        let engine = context
            .guard
            .scope(
                &context.caller,
                &NamespaceOperation::describe(
                    &describe_request(false, None),
                    WorkflowTarget::workflow(&workflow_id()),
                ),
            )
            .await?;
        let resolved = resolve_run_id(engine.engine()?.as_ref(), &workflow_id(), None).await?;

        assert_eq!(resolved, latest);
        Ok(())
    }

    #[tokio::test]
    async fn supplied_run_id_takes_precedence_over_latest_chain_entry()
    -> Result<(), Box<dyn std::error::Error>> {
        let context = context().await?;
        let requested = RunId::new(uuid::Uuid::from_u128(10));
        let latest = RunId::new(uuid::Uuid::from_u128(12));
        context.ownership.record(workflow_id(), NAMESPACE)?;
        append_continued_chain(context.store.as_ref(), &requested, &latest).await?;

        let engine = context
            .guard
            .scope(
                &context.caller,
                &NamespaceOperation::describe(
                    &describe_request(false, Some(requested.clone())),
                    WorkflowTarget::workflow(&workflow_id()),
                ),
            )
            .await?;
        let resolved = resolve_run_id(
            engine.engine()?.as_ref(),
            &workflow_id(),
            Some(requested.clone().into()),
        )
        .await?;

        assert_eq!(resolved, requested);
        Ok(())
    }
}
