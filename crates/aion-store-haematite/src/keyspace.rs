//! Key-encoding scheme for the single-node haematite-backed store.
//!
//! The store keeps workflow-history events in haematite's native event-stream
//! keyspace and every other durable record (timers, packages, routes, and
//! outbox rows) in the general KV keyspace. The two keyspaces are disjoint by
//! construction: event streams are addressed through [`haematite::EventStore`]
//! (which encodes `stream_key || 0x00 || seq`), while the KV records here all
//! carry a single-byte region tag that is never `0x00` and never collides with
//! an event stream key. Workflows are enumerated directly from the event
//! streams (see [`workflow_id_from_event_stream_key`]), so there is no separate
//! workflow-id index.
//!
//! # Region tags
//!
//! | tag  | region                | key layout                                  |
//! |------|-----------------------|---------------------------------------------|
//! | `E`  | event stream          | `E || workflow_uuid_bytes` (16 bytes)       |
//! | `t`  | durable timers        | `t: || workflow_id_text || 0x1f || timer`   |
//! | `p`  | deployed packages     | `p: || type || 0x1f || content_hash`        |
//! | `r`  | package routes        | `r: || workflow_type`                       |
//! | `o`  | outbox rows           | `o: || dispatch_key`                        |
//!
//! Range scans over a tag prefix use the half-open upper bound produced by
//! [`prefix_upper_bound`]. Because the single-node store runs haematite with
//! `shard_count == 1`, every key lives in one shard and a prefix range scan is
//! globally complete (haematite's `range` is shard-local, routed from `from`).

use aion_core::WorkflowId;
use uuid::Uuid;

/// Field separator inside composite KV keys (ASCII unit separator).
///
/// `0x1f` sorts below every printable byte used in a workflow type, content
/// hash, or timer-id token, so a composite key never lets one field's tail bleed
/// into the next field's head when range-scanning a prefix.
pub(crate) const FIELD_SEP: u8 = 0x1f;

/// Event-stream key for `workflow_id`: the tag byte `E` followed by the raw
/// 16-byte UUID.
///
/// This is handed to [`haematite::EventStore::append_batch`]/`read`, which append
/// their own `0x00 || seq` suffix. The `E` tag keeps the stream key out of the
/// `b"_:"` KV regions below.
pub(crate) fn event_stream_key(workflow_id: &WorkflowId) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + 16);
    key.push(b'E');
    key.extend_from_slice(workflow_id.as_uuid().as_bytes());
    key
}

/// Recover the [`WorkflowId`] from an event-stream key, if `key` is one.
///
/// The inverse of [`event_stream_key`]: a stream key is the tag byte `E`
/// followed by the raw 16-byte UUID, so a valid key is exactly 17 bytes long
/// and starts with `E`. Returns `None` for any other key (e.g. a KV-region key
/// or a malformed stream key), so callers enumerating workflows from the event
/// streams can defensively skip non-stream keys.
pub(crate) fn workflow_id_from_event_stream_key(key: &[u8]) -> Option<WorkflowId> {
    if key.len() != 17 || key[0] != b'E' {
        return None;
    }
    Uuid::from_slice(&key[1..17]).ok().map(WorkflowId::new)
}

/// Prefix for the durable-timer region.
pub(crate) const TIMER_PREFIX: &[u8] = b"t:";

/// Timer key for `(workflow_id, timer_id_token)`.
pub(crate) fn timer_key(workflow_id: &WorkflowId, timer_id_token: &str) -> Vec<u8> {
    composite(
        TIMER_PREFIX,
        &[workflow_id.to_string().as_bytes(), timer_id_token.as_bytes()],
    )
}

/// Prefix for the deployed-package region.
pub(crate) const PACKAGE_PREFIX: &[u8] = b"p:";

/// Package key for `(workflow_type, content_hash)`.
pub(crate) fn package_key(workflow_type: &str, content_hash: &str) -> Vec<u8> {
    composite(
        PACKAGE_PREFIX,
        &[workflow_type.as_bytes(), content_hash.as_bytes()],
    )
}

/// Prefix for the package-route region.
pub(crate) const ROUTE_PREFIX: &[u8] = b"r:";

/// Route key for `workflow_type`.
pub(crate) fn route_key(workflow_type: &str) -> Vec<u8> {
    composite(ROUTE_PREFIX, &[workflow_type.as_bytes()])
}

/// Decode a route key back into its workflow type.
pub(crate) fn workflow_type_from_route_key(key: &[u8]) -> Option<String> {
    let suffix = key.strip_prefix(ROUTE_PREFIX)?;
    String::from_utf8(suffix.to_vec()).ok()
}

/// Prefix for the outbox region.
pub(crate) const OUTBOX_PREFIX: &[u8] = b"o:";

/// Outbox key for `dispatch_key`.
pub(crate) fn outbox_key(dispatch_key: &str) -> Vec<u8> {
    composite(OUTBOX_PREFIX, &[dispatch_key.as_bytes()])
}

/// Build a composite key: `prefix` then each field joined by [`FIELD_SEP`].
fn composite(prefix: &[u8], fields: &[&[u8]]) -> Vec<u8> {
    let mut key = prefix.to_vec();
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            key.push(FIELD_SEP);
        }
        key.extend_from_slice(field);
    }
    key
}

/// The exclusive upper bound for a half-open `[prefix, upper)` range scan that
/// returns exactly the keys starting with `prefix`.
///
/// Increments the last byte; if every byte is `0xff` (no finite successor)
/// returns `None`, and callers treat that as an unbounded tail. A `0xff`-only
/// prefix never occurs for the ASCII region tags this store uses.
pub(crate) fn prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut upper = prefix.to_vec();
    while let Some(last) = upper.last_mut() {
        if *last < 0xff {
            *last += 1;
            return Some(upper);
        }
        upper.pop();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_core::WorkflowId;
    use uuid::Uuid;

    #[test]
    fn workflow_id_from_event_stream_key_round_trips() {
        let workflow_id = WorkflowId::new(Uuid::from_u128(42));
        let key = event_stream_key(&workflow_id);
        assert_eq!(
            workflow_id_from_event_stream_key(&key),
            Some(workflow_id)
        );
    }

    #[test]
    fn workflow_id_from_event_stream_key_rejects_non_stream_keys() {
        // A KV-region key (wrong tag / wrong length) is not a stream key.
        assert_eq!(workflow_id_from_event_stream_key(timer_key(
            &WorkflowId::new(Uuid::from_u128(1)),
            "t"
        ).as_slice()), None);
        // A 17-byte key with the wrong tag byte is rejected.
        let mut wrong_tag = vec![b'X'];
        wrong_tag.extend_from_slice(Uuid::from_u128(2).as_bytes());
        assert_eq!(workflow_id_from_event_stream_key(&wrong_tag), None);
    }

    #[test]
    fn route_key_round_trips() {
        let key = route_key("checkout");
        assert_eq!(workflow_type_from_route_key(&key).as_deref(), Some("checkout"));
    }

    #[test]
    fn prefix_upper_bound_increments_last_byte() {
        assert_eq!(prefix_upper_bound(b"t:"), Some(b"t;".to_vec()));
    }

    #[test]
    fn prefix_upper_bound_unbounded_when_all_ff() {
        assert_eq!(prefix_upper_bound(&[0xff, 0xff]), None);
    }

    #[test]
    fn event_stream_key_is_tagged_and_disjoint_from_kv_regions() {
        let workflow_id = WorkflowId::new(Uuid::from_u128(7));
        let key = event_stream_key(&workflow_id);
        assert_eq!(key[0], b'E');
        assert_ne!(&key[..2], TIMER_PREFIX);
        assert_ne!(&key[..2], OUTBOX_PREFIX);
    }
}
