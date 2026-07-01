//! A generic, harness-neutral JSON-RPC 2.0 over newline-delimited stdio helper.
//!
//! This is the reusable building block **any** stdio-JSON-RPC harness adapter reuses instead of
//! hand-rolling framing (§3A.2 / §9.4). It is machinery, **not a harness**: it names no concrete
//! harness and no method namespace. The concrete adapter (e.g. the Norn adapter in
//! `aion-integration-norn`) builds on this to speak its own methods.
//!
//! It provides:
//!
//! - the generic [`JsonRpcRequest`] / [`JsonRpcResponse`] / [`JsonRpcNotification`] envelopes,
//!   with request-vs-notification discrimination on `Option<id>`,
//! - the standard JSON-RPC 2.0 [error codes](error_codes),
//! - [`JsonRpcConnection`]: newline-delimited framing over any async duplex, a **single
//!   serializing writer** (so responses and outbound notifications never interleave-corrupt a
//!   frame), and request-id correlation.
//!
//! The layer is transport-agnostic over any [`tokio::io::AsyncRead`] + [`tokio::io::AsyncWrite`]
//! pair (a child's stdout/stdin, an in-memory duplex in tests, a socket) — it never assumes the
//! process's own stdio.

mod envelope;
mod transport;

pub use envelope::{
    IncomingMessage, JsonRpcError, JsonRpcId, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    error_codes,
};
pub use transport::JsonRpcConnection;
