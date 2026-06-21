//! Local dev-server surface with production parity.
//!
//! The dev surface gives an author a faithful local UI over the REAL engine,
//! store, and event stream — never a mock-only engine and never an execution
//! path whose semantics diverge from production (CN4). It is dark by default,
//! gated on `[dev].enabled`, exactly like the deploy and authoring surfaces.
//!
//! The surface is four operations, each built on existing infrastructure:
//!
//! * trigger a run — the same start path `/workflows/start` drives;
//! * stream that run's events — the existing `/events/stream` WebSocket
//!   firehose, reused (no second stream is built); the trigger response carries
//!   the exact per-workflow subscription frame;
//! * mock a named activity per-run — a shared [`ActivityMockRegistry`] the
//!   engine's dispatcher already consults via [`DevMockingDispatcher`], so the
//!   engine is untouched;
//! * replay a failed run — re-drive it through the real engine and store.

/// Transport-agnostic dev-server handlers.
pub mod handlers;
/// Opt-in per-run activity mocking layered over the production dispatcher.
pub mod mock;

pub use handlers::{
    MockOutcome, RegisterMockRequest, RegisterMockResponse, ReplayRunRequest, ReplayRunResponse,
    StreamSubscription, TriggerRunRequest, TriggerRunResponse, register_mock, replay_run,
    trigger_run,
};
pub use mock::{ActivityMockRegistry, DevMockingDispatcher, MockedActivity};
