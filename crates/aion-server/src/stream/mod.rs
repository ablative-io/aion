//! Event subscription streaming surfaces.

/// Namespace-aware event gating at the broadcast/encode seam.
pub mod namespace_filter;
/// Replay/live splice for per-workflow subscription resumption.
pub mod resume;
/// Server-side workflow-type/status selector filtering.
pub mod selector;
/// WebSocket frame encoding and subscription forwarding helpers.
pub mod socket;
/// Event subscription request mapping and store subscription helpers.
pub mod subscribe;

pub use namespace_filter::{GateVerdict, NamespaceEventGate};
pub use resume::RESUME_CURSOR_AHEAD_OF_HISTORY;
pub use selector::SubscriptionSelector;
pub use socket::{
    EncodedEventStream, EncodedFrame, SEQUENCE_CONTIGUITY_VIOLATION, forward_subscription,
    handle_subscription_socket,
};
pub use subscribe::{
    EventSubscription, MappedSubscription, map_subscription_request, subscribe_events,
};
