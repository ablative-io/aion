//! axum HTTP/JSON workflow facade.
//!
//! Module layout:
//! - [`router`] — public router construction (the only public surface).
//! - `workflows` — workflow management handlers.
//! - `schedules` — schedule management handlers.
//! - `events` — websocket event-subscription handlers.
//! - `deploy` — operator deploy handlers (mounted only when `[deploy].enabled`).
//! - `auth` — caller-identity extraction from request headers.
//! - `visibility` — visibility query-string parsing and namespace scoping.
//! - `payload` — HTTP body/payload encode-decode shapes and conversions.
//! - `error` — wire-error-to-HTTP response mapping.

mod auth;
mod deploy;
mod error;
mod events;
mod payload;
mod router;
mod schedules;
mod visibility;
mod workflows;

#[cfg(test)]
mod test_support;

pub use router::{http_router, workflow_router};
