//! Event subscription streaming surfaces.

/// WebSocket frame encoding and subscription forwarding helpers.
pub mod socket;
/// Event subscription request mapping and store subscription helpers.
pub mod subscribe;

pub use socket::{
    EncodedEventStream, EncodedFrame, forward_subscription, handle_subscription_socket,
};
pub use subscribe::{
    EventSubscription, MappedSubscription, map_subscription_request, subscribe_events,
};
