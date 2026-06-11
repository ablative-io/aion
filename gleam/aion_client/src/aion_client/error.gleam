//// Shared caller-side SDK error taxonomy.

/// Closed error taxonomy from the language-neutral client contract.
pub type Error {
  NotFound
  AlreadyExists
  QueryFailed
  QueryTimeout
  Cancelled
  Unavailable
  Unauthenticated
  /// The caller's credential was accepted, but the caller has no grant for
  /// the requested namespace. Workflow-level invisibility (a nonexistent or
  /// foreign-owned workflow) surfaces as `NotFound`, never as this variant,
  /// so cross-namespace existence is not leaked. Distinct from
  /// `Unauthenticated` (credential failure) and from `InvalidArgument`; not
  /// retryable until grants or the request change. Carries the server's
  /// detail message.
  NamespaceDenied(detail: String)
  InvalidArgument
  Server(detail: String)
}

/// AW wire error codes that can be decoded by transports before mapping them
/// to the closed caller-side taxonomy. Mirrors the ten codes of the wire
/// error enum in `crates/aion-proto/proto/common.proto` exactly; `WireUnknown`
/// preserves any code this SDK does not recognise.
pub type WireErrorCode {
  WireNotFound
  WireNamespaceDenied
  WireSequenceConflict
  WireUnknownQuery
  WireQueryTimeout
  WireNotRunning
  WireLagged
  WireInvalidInput
  WireBackend
  WireQueryFailed
  WireUnknown(code: String)
}

/// Map an AW wire error code into the shared client taxonomy.
///
/// `WireSequenceConflict` is the server's internal double-writer-bug signal
/// (a single-writer invariant violation), never an idempotency outcome, so it
/// surfaces as `Server` rather than `AlreadyExists`.
pub fn from_wire(code: WireErrorCode, detail: String) -> Error {
  case code {
    WireNotFound -> NotFound
    WireNamespaceDenied -> NamespaceDenied(detail)
    WireSequenceConflict -> Server(detail)
    WireUnknownQuery -> InvalidArgument
    WireQueryTimeout -> QueryTimeout
    WireNotRunning -> InvalidArgument
    WireLagged -> Unavailable
    WireInvalidInput -> InvalidArgument
    WireBackend -> Server(detail)
    WireQueryFailed -> QueryFailed
    WireUnknown(_) -> Server(detail)
  }
}

/// Map HTTP status codes used by aion-server into the shared taxonomy.
/// aion-server returns 401 exclusively for credential failure and 403
/// exclusively for namespace denial. Gateway statuses 502/503/504 signal
/// transient unreachability and map to the retryable `Unavailable`; 500 and
/// any unrecognised status are server faults.
pub fn from_http_status(status: Int, detail: String) -> Error {
  case status {
    400 -> InvalidArgument
    401 -> Unauthenticated
    403 -> NamespaceDenied(detail)
    404 -> NotFound
    408 -> QueryTimeout
    409 -> AlreadyExists
    412 -> InvalidArgument
    429 -> Unavailable
    499 -> Cancelled
    500 -> Server(detail)
    502 | 503 | 504 -> Unavailable
    _ -> Server(detail)
  }
}

/// Transport, DNS, TLS, socket, and transient stream failures are retryable
/// availability failures in the contract.
pub fn transport_failure() -> Error {
  Unavailable
}
