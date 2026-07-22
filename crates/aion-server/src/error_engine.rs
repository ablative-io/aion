//! Focused wire mappings for nested engine error families.

use aion::EngineError;
use aion::durability::DurabilityError;
use aion_proto::WireError;

pub(super) fn invalid_state_wire(reason: &str) -> WireError {
    WireError::invalid_state_with_type("InvalidState", reason.to_owned())
}

pub(super) fn backend_wire(error_type: &'static str, source: &EngineError) -> WireError {
    WireError::backend_with_type(error_type, source.to_string())
}

pub(super) fn durability_wire(durability: &DurabilityError, source: &EngineError) -> WireError {
    match durability {
        DurabilityError::Store(store) => super::wire_from_store(store),
        DurabilityError::NonDeterminism(_)
        | DurabilityError::HistoryShape { .. }
        | DurabilityError::SearchAttribute(_) => {
            WireError::backend_with_type("Durability", source.to_string())
        }
    }
}

/// Trace and wire discriminator for live-query dispatch failures.
pub(super) fn query_error_type(source: &aion::QueryError) -> &'static str {
    match source {
        aion::QueryError::UnknownQuery(_) => "UnknownQuery",
        aion::QueryError::Timeout => "QueryTimeout",
        aion::QueryError::NotRunning(_) => "QueryNotRunning",
        aion::QueryError::Unknown(_) => "QueryUnknownWorkflow",
        aion::QueryError::ReplyDropped => "QueryReplyDropped",
        aion::QueryError::HandlerFailed { .. } => "QueryFailed",
        aion::QueryError::Engine(_) => "QueryEngine",
    }
}

/// Wire mapping for live-query dispatch failures.
pub(super) fn query_wire(query: &aion::QueryError, source: &EngineError) -> WireError {
    match query {
        aion::QueryError::UnknownQuery(_) => WireError::unknown_query(source.to_string()),
        aion::QueryError::Timeout => WireError::query_timeout(source.to_string()),
        aion::QueryError::NotRunning(_) | aion::QueryError::ReplyDropped => {
            WireError::not_running_with_type(query_error_type(query), source.to_string())
        }
        aion::QueryError::Unknown(_) => {
            WireError::not_found_with_type(query_error_type(query), source.to_string())
        }
        aion::QueryError::HandlerFailed { .. } => {
            WireError::query_failed(source.to_string()).with_error_type(query_error_type(query))
        }
        aion::QueryError::Engine(_) => {
            WireError::backend_with_type(query_error_type(query), source.to_string())
        }
    }
}
