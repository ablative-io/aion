//! Error mapping from haematite's API/database errors into Aion's [`StoreError`].

use aion_store::StoreError;

/// Map a haematite [`haematite::ApiError`] into Aion's [`StoreError`].
///
/// A haematite append `SequenceConflict` is the same optimistic-concurrency
/// signal as Aion's `SequenceConflict`; everything else is a backend boundary
/// failure. Note the conformance contract reports the *observed head* in
/// `found`, which the append path supplies directly rather than reading it from
/// the haematite error (haematite's `actual` is its own next-seq).
pub(crate) fn api_error(error: &haematite::ApiError) -> StoreError {
    StoreError::Backend(format!("haematite api error: {error}"))
}

/// Map a haematite [`haematite::DatabaseError`] into Aion's [`StoreError`].
pub(crate) fn database_error(error: &haematite::DatabaseError) -> StoreError {
    StoreError::Backend(format!("haematite database error: {error}"))
}

/// Map a `serde_json` failure into Aion's [`StoreError::Serialization`].
pub(crate) fn serde_error(error: &serde_json::Error) -> StoreError {
    StoreError::Serialization(error.to_string())
}

/// Map a `tokio` `spawn_blocking` join failure into a backend error.
pub(crate) fn join_error(error: &tokio::task::JoinError) -> StoreError {
    StoreError::Backend(format!("haematite store blocking task failed: {error}"))
}
