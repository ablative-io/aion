//! Durable, minted-on-use namespace registry contract (Control-Plane Phase 1).
//!
//! Turns the namespace from a free-form per-workflow label into a first-class
//! durable record so that the live set of namespaces is listable and survives
//! owner-node death / failover. A namespace comes into being with zero
//! ceremony: a worker registering for an unseen namespace mints one via an
//! idempotent upsert — no pre-provision step.
//!
//! Existence is anchored on durable **state**, never on the registry row
//! alone: a namespace exists if it has durable state OR a live worker OR an
//! explicit registry entry. Worker-minting is one path to existence, never the
//! definition, so a reaped row can never orphan durable history.
//!
//! This module defines the foundation only: the [`NamespaceRecord`] shape, its
//! opaque-byte codec, and the [`NamespaceStore`] trait. The store backends
//! (in-memory / libSQL local-only, haematite quorum-replicated) implement the
//! trait in later slices; the store treats the record as opaque truth and only
//! decodes it to satisfy `list`.

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::StoreError;

/// One durable namespace registry entry.
///
/// The control-plane source of truth for "this namespace exists": listable,
/// failover-survivable, and the anchor for future per-namespace policy
/// (quotas, placement, retention).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceRecord {
    /// The namespace name. Free-form, exactly as carried on the wire
    /// (`StartWorkflowRequest.namespace` / `RegisterWorker.namespaces`).
    /// Primary key.
    pub name: String,
    /// When the registry first minted this namespace (first reference).
    pub created_at: DateTime<Utc>,
    /// Most recent time a worker/start referenced it — refreshed on
    /// mint-touch. Drives staleness/observability; never drives reaping while
    /// durable state exists.
    pub last_seen: DateTime<Utc>,
    /// How it came to exist: worker-mint, explicit POST, or
    /// inferred-from-state.
    pub origin: NamespaceOrigin,
    /// Reserved per-namespace policy blob (retention, quotas, auth scope).
    /// Phase 1 writes [`NamespaceConfig::default`]; Phase 2 fills it. Present
    /// day-one to avoid a later data migration.
    pub config: NamespaceConfig,
    /// Reserved placement directive (node/shard-range affinity). Phase 1 is
    /// [`NamespacePlacement::Unplaced`]. Present day-one so physical isolation
    /// is a later policy, not a migration.
    pub placement: NamespacePlacement,
    /// Lifecycle state, so a namespace can be retired
    /// (deprecate-before-delete) without losing its durable history.
    pub state: NamespaceState,
}

/// How a namespace came to exist in the registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NamespaceOrigin {
    /// Minted by a worker registering for a previously unseen namespace.
    WorkerMint,
    /// Created by an explicit operator request (`POST /namespaces`).
    Explicit,
    /// Back-filled lazily because durable state existed without a registry
    /// row (e.g. a pre-upgrade namespace).
    InferredFromState,
}

/// Lifecycle state of a namespace record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NamespaceState {
    /// In service.
    Active,
    /// Retired-but-retained: no new work should target it, but its durable
    /// history is preserved (deprecate-before-delete).
    Deprecated,
}

/// Result of a minted-on-use upsert: whether this call brought the record into
/// being or observed one that already existed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MintOutcome {
    /// This call created the record (drives the "loud created" signal).
    Created,
    /// A record already existed (idempotent touch / concurrent racer won).
    AlreadyExisted,
}

/// Reserved per-namespace policy blob.
///
/// Empty in Phase 1 (writes [`Default`]); Phase 2 fills retention, quotas, and
/// auth-scope keys. Reserving it day-one makes those later additions a policy
/// flip rather than a data migration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamespaceConfig {
    /// Reserved tenant / sub-grouping discriminator (`namespace-IS-tenant` vs
    /// a sub-grouping of a larger tenant).
    ///
    /// **Reserved for Phase 2.** Always `None` in Phase 1. Reserved now so the
    /// tenant⊃namespace split can be introduced later as a policy flip, not a
    /// record-shape migration.
    pub kind: Option<String>,
}

/// Reserved placement directive for a namespace.
///
/// Phase 1 is always [`NamespacePlacement::Unplaced`]. Later phases add
/// node-affinity / shard-range variants so physical isolation becomes a policy
/// rather than a record-shape migration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum NamespacePlacement {
    /// No placement directive — the namespace's records scatter across shards
    /// by name-hash like all other durable state.
    #[default]
    Unplaced,
}

impl NamespaceRecord {
    /// Builds a freshly minted record for `name`.
    ///
    /// `created_at` and `last_seen` are both set to `now` (a brand-new
    /// namespace has been seen exactly once, at creation), `state` is
    /// [`NamespaceState::Active`], and `config`/`placement` take their
    /// reserved Phase-1 defaults.
    #[must_use]
    pub fn new_minted(name: &str, origin: NamespaceOrigin, now: DateTime<Utc>) -> Self {
        Self {
            name: name.to_owned(),
            created_at: now,
            last_seen: now,
            origin,
            config: NamespaceConfig::default(),
            placement: NamespacePlacement::default(),
            state: NamespaceState::Active,
        }
    }

    /// Advances `last_seen` to `now`, leaving every other field untouched.
    ///
    /// Used by the idempotent mint-touch path: re-referencing an existing
    /// namespace refreshes its staleness signal without altering existence,
    /// origin, or lifecycle state. `now` is applied unconditionally — callers
    /// supply a monotonic clock.
    pub fn bump_last_seen(&mut self, now: DateTime<Utc>) {
        self.last_seen = now;
    }

    /// Encodes the record to opaque bytes for store persistence.
    ///
    /// Mirrors the package codec: `serde_json` over a stable on-disk form with
    /// instants rendered as RFC 3339 text. The store backend never parses the
    /// result beyond [`NamespaceRecord::decode`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Serialization`] if the record cannot be encoded.
    pub fn encode(&self) -> Result<Vec<u8>, StoreError> {
        let stored = StoredNamespace {
            name: self.name.clone(),
            created_at: encode_instant(self.created_at),
            last_seen: encode_instant(self.last_seen),
            origin: self.origin,
            kind: self.config.kind.clone(),
            placement: self.placement.clone(),
            state: self.state,
        };
        serde_json::to_vec(&stored).map_err(|error| StoreError::Serialization(error.to_string()))
    }

    /// Decodes a record previously produced by [`NamespaceRecord::encode`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Serialization`] if `bytes` is not a valid encoded
    /// record (malformed JSON or an unparseable instant).
    pub fn decode(bytes: &[u8]) -> Result<Self, StoreError> {
        let stored: StoredNamespace = serde_json::from_slice(bytes)
            .map_err(|error| StoreError::Serialization(error.to_string()))?;
        Ok(Self {
            name: stored.name,
            created_at: decode_instant(&stored.created_at)?,
            last_seen: decode_instant(&stored.last_seen)?,
            origin: stored.origin,
            config: NamespaceConfig { kind: stored.kind },
            placement: stored.placement,
            state: stored.state,
        })
    }
}

/// On-disk form of a [`NamespaceRecord`].
///
/// Instants are rendered as RFC 3339 text (matching the package/timer
/// encodings) so the persisted form is backend-agnostic and human-legible.
#[derive(Serialize, Deserialize)]
struct StoredNamespace {
    name: String,
    created_at: String,
    last_seen: String,
    origin: NamespaceOrigin,
    kind: Option<String>,
    placement: NamespacePlacement,
    state: NamespaceState,
}

impl Serialize for NamespacePlacement {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Unplaced => serializer.serialize_str("unplaced"),
        }
    }
}

impl<'de> Deserialize<'de> for NamespacePlacement {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let tag = String::deserialize(deserializer)?;
        match tag.as_str() {
            "unplaced" => Ok(Self::Unplaced),
            other => Err(serde::de::Error::custom(format!(
                "unknown namespace placement: {other}"
            ))),
        }
    }
}

fn encode_instant(instant: DateTime<Utc>) -> String {
    instant.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_instant(value: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|date_time| date_time.with_timezone(&Utc))
        .map_err(|error| StoreError::Serialization(error.to_string()))
}

/// Durable persistence contract for the minted-on-use namespace registry.
///
/// A sibling to [`crate::PackageStore`], deliberately *not* folded into it: the
/// registry's durability is stronger (it must survive owner-node death via the
/// quorum-replicated path, where packages use a plain local write), and its
/// create-if-absent / value-CAS / reconcile-on-conflict mint semantics have no
/// analogue in the package store's unconditional `put`. Single-node / local
/// backends satisfy the contract with a plain local upsert (no quorum to
/// reach); the haematite backend implements the real quorum-replicated path.
#[async_trait]
pub trait NamespaceStore: Send + Sync + 'static {
    /// Idempotent minted-on-use upsert.
    ///
    /// Create-if-absent: if no record exists for `name`, one is minted with
    /// the given `origin`. If a record already exists, its `last_seen` is
    /// refreshed (value-CAS touch) and `origin` is left untouched. A concurrent
    /// racer that wrote an equivalent record first is reconciled as success —
    /// the mint is idempotent and lock-free.
    ///
    /// Returns whether this call CREATED the record (drives the "loud created"
    /// event) versus touched an existing one.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotOwner`] if a quorum write is fenced because
    /// this node is not the current owner of the record's shard, or
    /// [`StoreError::Backend`] / [`StoreError::Serialization`] on a backend or
    /// codec failure.
    async fn register_namespace(
        &self,
        name: &str,
        origin: NamespaceOrigin,
    ) -> Result<MintOutcome, StoreError>;

    /// Explicit upsert (`POST /namespaces`).
    ///
    /// The same idempotent upsert as [`NamespaceStore::register_namespace`],
    /// but carrying a caller-supplied `record` (typically
    /// [`NamespaceOrigin::Explicit`] with an initial config). Idempotent on an
    /// existing name: an already-present record is reconciled as success
    /// rather than overwritten wholesale.
    ///
    /// # Errors
    ///
    /// As [`NamespaceStore::register_namespace`].
    async fn put_namespace(&self, record: NamespaceRecord) -> Result<MintOutcome, StoreError>;

    /// Returns the live durable set, ascending by `created_at` (ties broken by
    /// `name`).
    ///
    /// Backs `GET /namespaces`. The returned set is the raw durable truth;
    /// grant-filtering happens at the API layer, never here.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] / [`StoreError::Serialization`] on a
    /// backend or codec failure.
    async fn list_namespaces(&self) -> Result<Vec<NamespaceRecord>, StoreError>;

    /// Looks up a single namespace by `name`.
    ///
    /// The existence probe for the `closed` auto-create policy and the
    /// resolver's existence anchor. Returns `None` for an absent name (never an
    /// error).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Backend`] / [`StoreError::Serialization`] on a
    /// backend or codec failure.
    async fn get_namespace(&self, name: &str) -> Result<Option<NamespaceRecord>, StoreError>;

    /// Transitions a namespace from [`NamespaceState::Active`] to
    /// [`NamespaceState::Deprecated`] (deprecate-before-delete).
    ///
    /// Idempotent: deprecating an already-deprecated namespace, or one with no
    /// registry row, is a no-op rather than an error. Deprecation never strands
    /// durable history.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotOwner`] if a quorum write is fenced, or
    /// [`StoreError::Backend`] / [`StoreError::Serialization`] on a backend or
    /// codec failure.
    async fn deprecate_namespace(&self, name: &str) -> Result<(), StoreError>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{
        MintOutcome, NamespaceConfig, NamespaceOrigin, NamespacePlacement, NamespaceRecord,
        NamespaceState,
    };
    use chrono::{Duration, TimeZone, Utc};

    fn fixed_now() -> chrono::DateTime<Utc> {
        match Utc.with_ymd_and_hms(2026, 6, 30, 12, 0, 0).single() {
            Some(instant) => instant,
            None => Utc::now(),
        }
    }

    #[test]
    fn new_minted_sets_created_equal_to_last_seen_and_origin() {
        let now = fixed_now();
        let record = NamespaceRecord::new_minted("orders", NamespaceOrigin::WorkerMint, now);

        assert_eq!(record.name, "orders");
        assert_eq!(record.created_at, now);
        assert_eq!(record.last_seen, now);
        assert_eq!(record.created_at, record.last_seen);
        assert_eq!(record.origin, NamespaceOrigin::WorkerMint);
        assert_eq!(record.state, NamespaceState::Active);
        assert_eq!(record.config, NamespaceConfig::default());
        assert_eq!(record.placement, NamespacePlacement::Unplaced);
        assert_eq!(record.config.kind, None);
    }

    #[test]
    fn bump_last_seen_advances_only_last_seen() {
        let now = fixed_now();
        let mut record = NamespaceRecord::new_minted("orders", NamespaceOrigin::Explicit, now);
        let later = now + Duration::seconds(42);

        record.bump_last_seen(later);

        assert_eq!(record.last_seen, later);
        assert_eq!(record.created_at, now);
        assert_eq!(record.origin, NamespaceOrigin::Explicit);
        assert_eq!(record.state, NamespaceState::Active);
    }

    #[test]
    fn encode_decode_round_trips() {
        let now = fixed_now();
        let mut record =
            NamespaceRecord::new_minted("billing", NamespaceOrigin::InferredFromState, now);
        record.bump_last_seen(now + Duration::seconds(5));
        record.state = NamespaceState::Deprecated;

        let bytes = record.encode().expect("encode");
        let decoded = NamespaceRecord::decode(&bytes).expect("decode");

        assert_eq!(record, decoded);
    }

    #[test]
    fn encode_decode_preserves_reserved_kind_discriminator() {
        let now = fixed_now();
        let mut record = NamespaceRecord::new_minted("tenant-a", NamespaceOrigin::Explicit, now);
        record.config.kind = Some("tenant".to_owned());

        let bytes = record.encode().expect("encode");
        let decoded = NamespaceRecord::decode(&bytes).expect("decode");

        assert_eq!(decoded.config.kind.as_deref(), Some("tenant"));
        assert_eq!(record, decoded);
    }

    #[test]
    fn decode_rejects_malformed_bytes() {
        let err = NamespaceRecord::decode(b"not json").expect_err("must reject");
        assert!(matches!(err, crate::StoreError::Serialization(_)));
    }

    #[test]
    fn enum_derives_are_copy_clone_eq() {
        // The trait signatures pass these by value / compare them, so they
        // must be Clone + Copy + PartialEq + Eq.
        let outcome = MintOutcome::Created;
        let copied = outcome;
        assert_eq!(outcome, copied);
        assert_eq!(copied, MintOutcome::Created);
        assert_ne!(MintOutcome::Created, MintOutcome::AlreadyExisted);

        let origin = NamespaceOrigin::WorkerMint;
        let origin_copy = origin;
        assert_eq!(origin, origin_copy);

        let state = NamespaceState::Active;
        let state_copy = state;
        assert_eq!(state, state_copy);
        assert_ne!(NamespaceState::Active, NamespaceState::Deprecated);
    }
}
