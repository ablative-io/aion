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
  InvalidArgument
  Server(detail: String)
}

/// AW wire error codes that can be decoded by transports before mapping them to
/// the closed caller-side taxonomy.
pub type WireErrorCode {
  WireNotFound
  WireNamespaceDenied
  WireSequenceConflict
  WireAlreadyExists
  WireUnknownQuery
  WireQueryFailed
  WireQueryTimeout
  WireNotRunning
  WireCancelled
  WireLagged
  WireBackend
  WireInvalidArgument
  WireUnauthenticated
  WireUnavailable
  WireUnknown(code: String)
}

/// Map an AW wire error code into the shared client taxonomy.
pub fn from_wire(code: WireErrorCode, detail: String) -> Error {
  case code {
    WireNotFound -> NotFound
    WireNamespaceDenied -> Unauthenticated
    WireSequenceConflict -> AlreadyExists
    WireAlreadyExists -> AlreadyExists
    WireUnknownQuery -> InvalidArgument
    WireQueryFailed -> QueryFailed
    WireQueryTimeout -> QueryTimeout
    WireNotRunning -> InvalidArgument
    WireCancelled -> Cancelled
    WireLagged -> Unavailable
    WireBackend -> Server(detail)
    WireInvalidArgument -> InvalidArgument
    WireUnauthenticated -> Unauthenticated
    WireUnavailable -> Unavailable
    WireUnknown(_) -> Server(detail)
  }
}

/// Map HTTP status codes used by aion-server into the shared taxonomy.
pub fn from_http_status(status: Int, detail: String) -> Error {
  case status {
    400 -> InvalidArgument
    401 -> Unauthenticated
    403 -> Unauthenticated
    404 -> NotFound
    408 -> QueryTimeout
    409 -> AlreadyExists
    412 -> InvalidArgument
    429 -> Unavailable
    499 -> Cancelled
    500 -> Server(detail)
    _ -> Server(detail)
  }
}

/// Transport, DNS, TLS, socket, and transient stream failures are retryable
/// availability failures in the contract.
pub fn transport_failure() -> Error {
  Unavailable
}
