//! Seam-assembly helpers used by `EngineBuilder::build()`: event-streaming
//! store wrapping and resolution of the final delegated seam bundle.

use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use aion_store::EventStore;

use crate::publish::PublishingEventStore;
use crate::signal::SignalResumeHandoff;
use crate::{EngineError, RuntimeHandle};

use super::delegated::{DelegatedSeams, EventPublisher, SignalRouter};

/// Deferred signal-router constructor applied once the runtime exists.
pub(super) type SignalRouterFactory = Arc<
    dyn Fn(Arc<RuntimeHandle>, Arc<SignalResumeHandoff>) -> Arc<dyn SignalRouter> + Send + Sync,
>;

/// Store and optional publisher produced by [`wrap_event_streaming`].
pub(super) type EventStreamingParts = (Arc<dyn EventStore>, Option<Arc<dyn EventPublisher>>);

/// Wrap the configured store for live event streaming when opted in.
///
/// The wrap happens before any recorder, recovery path, or NIF bridge clones
/// the store, so every append flows through the publishing wrapper.
pub(super) fn wrap_event_streaming(
    store: Arc<dyn EventStore>,
    capacity: Option<NonZeroUsize>,
    event_publisher_overridden: bool,
) -> Result<EventStreamingParts, EngineError> {
    let Some(capacity) = capacity else {
        return Ok((store, None));
    };
    if event_publisher_overridden {
        return Err(EngineError::ConflictingEventPublisher);
    }
    let publishing = PublishingEventStore::new(store, capacity)?;
    let publisher: Arc<dyn EventPublisher> = Arc::new(publishing.publisher());
    Ok((Arc::new(publishing), Some(publisher)))
}

/// Inputs for [`assemble_delegated_seams`], gathered once `build()` has the
/// runtime, signal handoff, streaming publisher, and query mailbox engine.
pub(super) struct SeamAssembly {
    pub(super) configured: DelegatedSeams,
    pub(super) signal_router_factory: Option<SignalRouterFactory>,
    pub(super) runtime: Arc<RuntimeHandle>,
    pub(super) signal_handoff: Arc<SignalResumeHandoff>,
    pub(super) streaming_publisher: Option<Arc<dyn EventPublisher>>,
    pub(super) query_mailbox_engine: Arc<dyn crate::engine_seam::EngineHandle>,
    pub(super) query_timeout: Option<Duration>,
    pub(super) query_service_overridden: bool,
}

/// Resolve the final delegated seams: signal-router factory application,
/// streaming-publisher install, and concrete query-service install.
pub(super) fn assemble_delegated_seams(assembly: SeamAssembly) -> DelegatedSeams {
    let delegated = if let Some(factory) = assembly.signal_router_factory {
        DelegatedSeams::new(
            factory(assembly.runtime, assembly.signal_handoff),
            assembly.configured.query_service_arc(),
            assembly.configured.event_publisher_arc(),
        )
    } else {
        assembly.configured
    };
    let delegated = install_streaming_publisher(delegated, assembly.streaming_publisher);
    install_concrete_query_service(
        delegated,
        assembly.query_mailbox_engine,
        assembly.query_timeout,
        assembly.query_service_overridden,
    )
}

/// Install the concrete query-dispatch seam when a query timeout was
/// configured and no explicit `.query_service(...)` override won.
///
/// Without a timeout the deferred seam stays installed (typed "not
/// configured" error) — no default timeout exists anywhere in the engine.
fn install_concrete_query_service(
    delegated: DelegatedSeams,
    query_mailbox_engine: Arc<dyn crate::engine_seam::EngineHandle>,
    query_timeout: Option<Duration>,
    query_service_overridden: bool,
) -> DelegatedSeams {
    match query_timeout {
        Some(timeout) if !query_service_overridden => DelegatedSeams::new(
            delegated.signal_router_arc(),
            Arc::new(crate::query::ConcreteQueryService::new(
                query_mailbox_engine,
                timeout,
            )),
            delegated.event_publisher_arc(),
        ),
        _ => delegated,
    }
}

/// Install the broadcast publisher built by [`wrap_event_streaming`] into the
/// delegated seams, keeping the configured signal and query seams.
fn install_streaming_publisher(
    delegated: DelegatedSeams,
    streaming_publisher: Option<Arc<dyn EventPublisher>>,
) -> DelegatedSeams {
    match streaming_publisher {
        Some(publisher) => DelegatedSeams::new(
            delegated.signal_router_arc(),
            delegated.query_service_arc(),
            publisher,
        ),
        None => delegated,
    }
}
