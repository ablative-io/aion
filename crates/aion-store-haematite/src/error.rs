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

/// Map a per-shard election outcome ([`haematite::Database::acquire_shard_and_serve`])
/// into Aion's [`StoreError`], recognizing the typed loss variants rather than
/// collapsing every failure into an opaque [`StoreError::Backend`].
///
/// The two outcomes are deliberately split (ADR-021 clean-partial):
///
/// * [`haematite::DatabaseError::ElectionLost`] — a strictly higher ballot was
///   promised elsewhere, so this candidate is NOT the owner. This is the
///   acquire-time twin of a fenced publish: it maps to the typed, retryable
///   [`StoreError::NotOwner`] so the adoption path can DROP this shard cleanly
///   (no extend, no recover) instead of treating a deposed survivor as a hard
///   durability failure. Mirrors `publish_shard_owner`'s `Fenced -> NotOwner`.
/// * Everything else (`ElectionTimeout`, quorum-unavailable, transport faults)
///   is a retryable boundary failure that says nothing about ownership: it stays
///   [`StoreError::Backend`] so the caller's existing retry contract is preserved
///   (a transient quorum gap must not be mistaken for "another node owns this").
pub(crate) fn acquire_election_error(error: &haematite::DatabaseError, shard: usize) -> StoreError {
    match error {
        // A higher ballot deposed this candidate: it is not the owner. Typed,
        // retryable routing signal — the acquire-side twin of a fenced publish.
        haematite::DatabaseError::ElectionLost { .. } => StoreError::NotOwner { shard },
        // ElectionTimeout / quorum-unavailable / transport: retryable, ownership
        // unknown. Keep the opaque backend mapping so the retry contract holds.
        other => database_error(other),
    }
}

/// Resolve a benign value-CAS conflict on a shard-owner publish (ADR-021): a
/// [`haematite::DatabaseError::CasConflict`] means this node is STILL the live
/// owner but a concurrent re-publish raced the value-hash precondition, so the
/// typed variants — not the value-hash CAS — discriminate supersession now.
///
/// Decision over the directory record re-read after the conflict:
///
/// * `recorded == Some(self_node_id)` — the record already names THIS node (the
///   concurrent writer published our own identity): idempotent, return `Ok(())`.
/// * `recorded == Some(other)` — a DIFFERENT node is recorded as owner: a genuine
///   ownership disagreement, surface [`StoreError::NotOwner`].
/// * `recorded == None` — no record landed locally yet despite the conflict:
///   nothing contradicts our ownership (we are still the fenced owner and a later
///   read converges), so treat the benign value-CAS loss as a no-op `Ok(())`.
pub(crate) fn resolve_cas_conflict(
    recorded: Option<&str>,
    self_node_id: &str,
    shard: usize,
) -> Result<(), StoreError> {
    match recorded {
        Some(owner) if owner == self_node_id => Ok(()),
        Some(_) => Err(StoreError::NotOwner { shard }),
        None => Ok(()),
    }
}

/// Map a `serde_json` failure into Aion's [`StoreError::Serialization`].
pub(crate) fn serde_error(error: &serde_json::Error) -> StoreError {
    StoreError::Serialization(error.to_string())
}

/// Map a `tokio` `spawn_blocking` join failure into a backend error.
pub(crate) fn join_error(error: &tokio::task::JoinError) -> StoreError {
    StoreError::Backend(format!("haematite store blocking task failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{acquire_election_error, resolve_cas_conflict};
    use aion_store::StoreError;

    /// A clean election loss (a strictly higher ballot deposed us) maps to the
    /// typed, retryable [`StoreError::NotOwner`] — the acquire-time twin of a
    /// fenced publish, so the adoption path can DROP the shard cleanly.
    #[test]
    fn election_lost_maps_to_not_owner() {
        let error = haematite::DatabaseError::ElectionLost { highest_seen: 9 };
        assert_eq!(
            acquire_election_error(&error, 4),
            StoreError::NotOwner { shard: 4 },
            "ElectionLost is a clean, droppable ownership loss → NotOwner"
        );
    }

    /// A quorum-unavailable election (or any non-loss fault) stays an opaque,
    /// retryable [`StoreError::Backend`] — it says NOTHING about ownership, so the
    /// caller's retry contract must be preserved.
    #[test]
    fn election_timeout_and_quorum_stay_backend() {
        for error in [
            haematite::DatabaseError::ElectionTimeout { attempts: 5 },
            haematite::DatabaseError::ConsistencyError("quorum cannot be reached".to_owned()),
        ] {
            assert!(
                matches!(acquire_election_error(&error, 1), StoreError::Backend(_)),
                "a quorum/transport election failure is retryable Backend, not NotOwner: {error}"
            );
        }
    }

    /// A value-CAS conflict whose re-read names THIS node is a benign idempotent
    /// re-publish (Ok); naming a DIFFERENT node is a real ownership loss
    /// (`NotOwner`); no record yet is a benign no-op (`Ok`).
    #[test]
    fn cas_conflict_is_idempotent_for_self_but_aborts_for_other() {
        assert_eq!(
            resolve_cas_conflict(Some("node-a"), "node-a", 0),
            Ok(()),
            "identical/own owner record → idempotent success"
        );
        assert_eq!(
            resolve_cas_conflict(Some("node-b"), "node-a", 0),
            Err(StoreError::NotOwner { shard: 0 }),
            "a DIFFERENT live owner recorded → abort (NotOwner)"
        );
        assert_eq!(
            resolve_cas_conflict(None, "node-a", 0),
            Ok(()),
            "no record landed yet → benign no-op success"
        );
    }
}
