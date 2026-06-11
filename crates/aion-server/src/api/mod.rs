//! Module declarations.

/// gRPC service implementation.
pub mod grpc;
/// Shared workflow operation handlers used by transports.
pub mod handlers;
/// HTTP and dashboard router construction.
pub mod http;
/// Shared schedule operation handlers used by transports.
pub mod schedule_handlers;
/// Remote-worker gRPC endpoint implementation.
pub mod worker_grpc;
