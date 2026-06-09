//! Module declarations.

/// gRPC service implementation.
pub mod grpc;
/// Shared workflow operation handlers used by transports.
pub mod handlers;
/// HTTP and dashboard router construction.
pub mod http;
/// Remote-worker gRPC endpoint implementation.
pub mod worker_grpc;
