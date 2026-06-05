//! TypeScript export tests for the dashboard generated-types pipeline.

use ts_rs::{Config, TS};

use crate::{
    ActivityError, ActivityErrorKind, ActivityId, ContentType, Event, EventEnvelope, Payload,
    RunId, TimerId, WorkflowError, WorkflowFilter, WorkflowId, WorkflowStatus, WorkflowSummary,
};

#[test]
fn export_dashboard_wire_types() -> Result<(), ts_rs::ExportError> {
    let config = Config::new()
        .with_out_dir("../../apps/aion-dashboard/src/types/generated")
        .with_large_int("number");

    ActivityError::export_all(&config)?;
    ActivityErrorKind::export_all(&config)?;
    ActivityId::export_all(&config)?;
    ContentType::export_all(&config)?;
    Event::export_all(&config)?;
    EventEnvelope::export_all(&config)?;
    Payload::export_all(&config)?;
    RunId::export_all(&config)?;
    TimerId::export_all(&config)?;
    WorkflowError::export_all(&config)?;
    WorkflowFilter::export_all(&config)?;
    WorkflowId::export_all(&config)?;
    WorkflowStatus::export_all(&config)?;
    WorkflowSummary::export_all(&config)?;

    Ok(())
}
