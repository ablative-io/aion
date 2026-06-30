//! Transport-agnostic minted-on-use namespace hook (Control-Plane Phase 1).
//!
//! The single, shared implementation of the auto-create policy: given an
//! already-authorized namespace set, durably mint each unseen namespace
//! ([`AutoCreate::Open`]) or gate it ([`AutoCreate::Closed`]). Both mint
//! choke-points reuse this one type so the policy can never diverge:
//!
//! - the worker-registration seam ([`crate::worker::registry::ConnectedWorkerRegistry`],
//!   the primary minter — S5), and
//! - the workflow-start seam ([`crate::api::handlers::start_with_placement`], the
//!   safety net for a client that starts before any worker registers — S6).
//!
//! In every case the mint runs strictly AFTER the caller's authorization, so it
//! can only ever record a namespace the caller is already permitted to use — the
//! mint is auth-scoped by construction (CVE-2025-14986: open minting and
//! namespace isolation only coexist when minting is auth-gated).

use std::sync::Arc;

use aion_core::ClusterEvent;
use aion_store::{MintOutcome, NamespaceOrigin, NamespaceStore};

use crate::cluster_publisher::ClusterEventPublisher;
use crate::config::AutoCreate;
use crate::error::ServerError;

/// Minted-on-use hook pairing the durable namespace registry with its
/// [`AutoCreate`] policy.
///
/// Holds an `Arc<dyn NamespaceStore>` and the policy, so it is cheap to clone
/// and share between the worker-registration and workflow-start mint seams. The
/// hook is the *only* place the auto-create policy is implemented; both seams
/// call [`NamespaceMinter::mint_or_gate`], so the behaviour can never diverge
/// across transports or call sites.
#[derive(Clone)]
pub struct NamespaceMinter {
    store: Arc<dyn NamespaceStore>,
    policy: AutoCreate,
    /// Optional ops-console push channel (WS3). When present, a genuinely-new
    /// namespace (a [`MintOutcome::Created`] edge) emits a durable
    /// [`ClusterEvent::NamespaceCreated`] delta on the SAME deploy-scoped
    /// channel that already carries the worker/peer/shard topology deltas — so
    /// the live namespace panel appends each namespace exactly once with no
    /// refresh. `None` keeps every existing construction (and every test) silent,
    /// exactly like the registry's other optional seams.
    cluster_publisher: Option<ClusterEventPublisher>,
}

impl std::fmt::Debug for NamespaceMinter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NamespaceMinter")
            .field("policy", &self.policy)
            .field("cluster_publisher", &self.cluster_publisher.is_some())
            .finish_non_exhaustive()
    }
}

impl NamespaceMinter {
    /// Build a minter over a durable namespace store and an auto-create policy.
    #[must_use]
    pub fn new(store: Arc<dyn NamespaceStore>, policy: AutoCreate) -> Self {
        Self {
            store,
            policy,
            cluster_publisher: None,
        }
    }

    /// Attach the WS3 cluster-event publisher so a first mint pushes a live
    /// `namespace created` delta to the ops console (Control-Plane Phase 1, S8).
    ///
    /// Pure builder addition: without it the minter behaves exactly as before
    /// (durable record + the `tracing` audit event only). The publisher is the
    /// deployment-global cluster channel — the same one the worker registry and
    /// supervisor emit on — so the delta reuses the existing browser push path
    /// rather than inventing a parallel channel.
    #[must_use]
    pub fn with_cluster_publisher(mut self, publisher: ClusterEventPublisher) -> Self {
        self.cluster_publisher = Some(publisher);
        self
    }

    /// The auto-create policy this minter applies.
    #[must_use]
    pub fn policy(&self) -> AutoCreate {
        self.policy
    }

    /// Apply the minted-on-use policy to an already-authorized namespace set.
    ///
    /// The caller MUST have authorized every namespace in `namespaces` before
    /// calling this — the mint is auth-scoped by construction, never a path to
    /// create a namespace the caller cannot use.
    ///
    /// - [`AutoCreate::Open`]: each namespace is durably upserted via
    ///   [`NamespaceStore::register_namespace`] with the given `origin`; a
    ///   [`MintOutcome::Created`] (first mint) emits a loud structured `tracing`
    ///   event — the Phase-1 "namespace created" signal (the socket-delta
    ///   surfacing lands in a later slice). A [`MintOutcome::AlreadyExisted`] is
    ///   silent (idempotent re-reference), so no duplicate row and no second
    ///   "created" event ever appear.
    /// - [`AutoCreate::Closed`]: a namespace with no registry row is rejected
    ///   with a namespace-denied error; nothing is created.
    ///
    /// A [`aion_store::StoreError::NotOwner`] from a quorum mint (this node is
    /// not the namespace shard's owner) propagates unchanged through `?` as
    /// [`ServerError::StoreBackend`], which surfaces as the typed, *retryable*
    /// `NotOwner` wire code — never a silent success.
    ///
    /// **Closed-policy existence check (Phase 1).** Existence is probed by
    /// registry-row presence ([`NamespaceStore::get_namespace`]). In a fresh
    /// Phase-1 deployment every used namespace already has a row minted on first
    /// register/start, so a missing row correctly means "never referenced".
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::StoreBackend`] if a durable upsert/lookup fails
    /// (including a retryable `NotOwner` fence), or [`ServerError::Namespace`]
    /// when `closed` rejects an unknown namespace.
    pub async fn mint_or_gate(
        &self,
        namespaces: &[String],
        origin: NamespaceOrigin,
    ) -> Result<(), ServerError> {
        for namespace in namespaces {
            match self.policy {
                AutoCreate::Open => {
                    if self.store.register_namespace(namespace, origin).await?
                        == MintOutcome::Created
                    {
                        self.announce_created(namespace, origin).await?;
                    }
                }
                AutoCreate::Closed => {
                    if self.store.get_namespace(namespace).await?.is_none() {
                        return Err(ServerError::namespace_denied(format!(
                            "namespace {namespace} does not exist and auto_create is closed"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Explicit operator create (`POST /namespaces`, S7) routed through the SAME
    /// `MintOutcome::Created` choke-point so the live "namespace created" delta
    /// fires once for an operator-minted namespace exactly as it does for a
    /// worker- or start-minted one.
    ///
    /// Unlike [`NamespaceMinter::mint_or_gate`] this never gates on the
    /// [`AutoCreate::Closed`] policy: an explicit operator create is the
    /// documented escape hatch that brings a namespace into being in a
    /// locked-down deployment. The caller MUST have authorized `name` first (the
    /// HTTP handler runs the grant check), so the create is auth-scoped by
    /// construction.
    ///
    /// Returns the [`MintOutcome`] so the handler can report created-vs-existing
    /// to the operator. Idempotent: a re-create observes `AlreadyExisted` and
    /// emits no second delta.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::StoreBackend`] if the durable upsert/lookup fails
    /// (including a retryable `NotOwner` fence).
    pub async fn create_explicit(&self, name: &str) -> Result<MintOutcome, ServerError> {
        let outcome = self
            .store
            .register_namespace(name, NamespaceOrigin::Explicit)
            .await?;
        if outcome == MintOutcome::Created {
            self.announce_created(name, NamespaceOrigin::Explicit)
                .await?;
        }
        Ok(outcome)
    }

    /// Emit the loud audit event AND (when a publisher is attached) the durable
    /// `namespace created` socket delta for a genuinely-new namespace.
    ///
    /// Called ONLY on the `MintOutcome::Created` edge, so it fires exactly once
    /// per genuinely-new namespace and never on an idempotent re-reference. The
    /// delta's `created_at` is read back from the durable record so the console's
    /// created column is the registry's authoritative instant, not a re-stamp at
    /// emit time; if the record cannot be re-read (a racer deprecated it, or a
    /// quorum hiccup) the audit event still fires and the delta is skipped rather
    /// than carrying a fabricated timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::StoreBackend`] only if the read-back lookup fails
    /// at the backend; an absent record (already reconciled away) is not an
    /// error — the audit event has already fired.
    async fn announce_created(
        &self,
        name: &str,
        origin: NamespaceOrigin,
    ) -> Result<(), ServerError> {
        tracing::info!(
            namespace = %name,
            origin = origin_label(origin),
            "namespace created"
        );
        let Some(publisher) = &self.cluster_publisher else {
            return Ok(());
        };
        let Some(record) = self.store.get_namespace(name).await? else {
            return Ok(());
        };
        let name = record.name;
        let created_at = record.created_at;
        let label = origin_label(record.origin).to_owned();
        drop(publisher.emit(move |meta| ClusterEvent::NamespaceCreated {
            meta,
            name,
            created_at,
            origin: label,
        }));
        Ok(())
    }
}

/// Stable `snake_case` label for the "namespace created" audit event, so the log
/// field stays the operational identifier (`worker_mint` / `start_mint` /
/// `explicit` / `inferred_from_state`) regardless of the enum's `Debug` form.
const fn origin_label(origin: NamespaceOrigin) -> &'static str {
    match origin {
        NamespaceOrigin::WorkerMint => "worker_mint",
        NamespaceOrigin::StartMint => "start_mint",
        NamespaceOrigin::Explicit => "explicit",
        NamespaceOrigin::InferredFromState => "inferred_from_state",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::num::NonZeroUsize;
    use std::sync::Arc;

    use aion_core::ClusterEvent;
    use aion_store::{InMemoryStore, NamespaceOrigin, NamespaceStore};
    use futures::StreamExt;

    use super::NamespaceMinter;
    use crate::cluster_publisher::ClusterEventPublisher;
    use crate::config::AutoCreate;

    fn publisher() -> ClusterEventPublisher {
        ClusterEventPublisher::new(NonZeroUsize::new(16).expect("non-zero capacity"))
    }

    fn open_minter(store: Arc<InMemoryStore>, publisher: ClusterEventPublisher) -> NamespaceMinter {
        let store: Arc<dyn NamespaceStore> = store;
        NamespaceMinter::new(store, AutoCreate::Open).with_cluster_publisher(publisher)
    }

    /// Pull the next delta off the stream, asserting it is a `NamespaceCreated`
    /// with the `explicit` origin label and returning its name.
    async fn next_created_name<S>(deltas: &mut S) -> Result<String, Box<dyn std::error::Error>>
    where
        S: futures::Stream<
                Item = Result<ClusterEvent, crate::cluster_publisher::ClusterStreamLagged>,
            > + Unpin,
    {
        let event = deltas
            .next()
            .await
            .ok_or("expected a namespace-created delta")?
            .map_err(|lag| format!("unexpected lag: {lag:?}"))?;
        match event {
            ClusterEvent::NamespaceCreated { name, origin, .. } => {
                assert_eq!(origin, "explicit");
                Ok(name)
            }
            other => Err(format!("expected NamespaceCreated, got {other:?}").into()),
        }
    }

    /// The single `MintOutcome::Created` choke-point pushes exactly one durable
    /// `NamespaceCreated` delta carrying the record's name + origin label, and an
    /// idempotent re-reference of the SAME namespace (an `AlreadyExisted` touch)
    /// pushes NOTHING — so the ops console appends each namespace exactly once
    /// with no refresh and no duplicate row.
    #[tokio::test]
    async fn namespace_created_delta_emits_once_on_created_and_not_on_already_existed()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(InMemoryStore::default());
        let publisher = publisher();
        let mut deltas = publisher.subscribe(0);
        let minter = open_minter(Arc::clone(&store), publisher);

        // First mint of a brand-new namespace: the Created edge.
        minter
            .mint_or_gate(&["orders".to_owned()], NamespaceOrigin::WorkerMint)
            .await?;

        let first = deltas
            .next()
            .await
            .ok_or("expected one namespace-created delta")?
            .map_err(|lag| format!("unexpected lag: {lag:?}"))?;
        match first {
            ClusterEvent::NamespaceCreated {
                name,
                origin,
                created_at,
                ..
            } => {
                assert_eq!(name, "orders");
                assert_eq!(origin, "worker_mint");
                // The carried instant is the durable record's own created_at.
                let record = store
                    .get_namespace("orders")
                    .await?
                    .ok_or("record must exist after a Created mint")?;
                assert_eq!(created_at, record.created_at);
            }
            other => return Err(format!("expected NamespaceCreated, got {other:?}").into()),
        }

        // Idempotent re-reference of the SAME namespace: an AlreadyExisted touch.
        // It must NOT emit a second delta. A different new namespace must, so we
        // can prove the channel is still live (the re-reference produced silence,
        // not a closed channel).
        minter
            .mint_or_gate(&["orders".to_owned()], NamespaceOrigin::WorkerMint)
            .await?;
        minter
            .mint_or_gate(&["billing".to_owned()], NamespaceOrigin::StartMint)
            .await?;

        let next = deltas
            .next()
            .await
            .ok_or("expected the second namespace's delta")?
            .map_err(|lag| format!("unexpected lag: {lag:?}"))?;
        match next {
            ClusterEvent::NamespaceCreated { name, origin, .. } => {
                // The very next delta is `billing`, proving the `orders`
                // re-reference emitted nothing in between (idempotent silence).
                assert_eq!(name, "billing");
                assert_eq!(origin, "start_mint");
            }
            other => return Err(format!("expected NamespaceCreated, got {other:?}").into()),
        }

        Ok(())
    }

    /// The explicit `POST /namespaces` path flows through the SAME choke-point, so
    /// an operator-minted namespace emits the `NamespaceCreated` delta once on
    /// create and is silent on an idempotent re-create.
    #[tokio::test]
    async fn explicit_create_emits_created_delta_once_then_silent_on_recreate()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = Arc::new(InMemoryStore::default());
        let publisher = publisher();
        let mut deltas = publisher.subscribe(0);
        let minter = open_minter(Arc::clone(&store), publisher);

        let created = minter.create_explicit("tenant-a").await?;
        assert_eq!(created, aion_store::MintOutcome::Created);
        // Idempotent re-create: AlreadyExisted, and no second delta.
        let again = minter.create_explicit("tenant-a").await?;
        assert_eq!(again, aion_store::MintOutcome::AlreadyExisted);

        // Emit one more genuinely-new namespace to bound the read: the next delta
        // proves the re-create was silent.
        let _ = minter.create_explicit("tenant-b").await?;

        let first = next_created_name(&mut deltas).await?;
        let second = next_created_name(&mut deltas).await?;
        assert_eq!(
            vec![first, second],
            vec!["tenant-a".to_owned(), "tenant-b".to_owned()]
        );

        Ok(())
    }

    /// Without a publisher attached the minter is silent (durable record + audit
    /// event only): the registry's other call sites that never wire the channel
    /// stay byte-identical, and minting never depends on a live subscriber.
    #[tokio::test]
    async fn mint_without_publisher_creates_record_but_emits_no_delta()
    -> Result<(), Box<dyn std::error::Error>> {
        let store: Arc<dyn NamespaceStore> = Arc::new(InMemoryStore::default());
        let minter = NamespaceMinter::new(Arc::clone(&store), AutoCreate::Open);

        minter
            .mint_or_gate(&["orders".to_owned()], NamespaceOrigin::WorkerMint)
            .await?;

        assert!(
            store.get_namespace("orders").await?.is_some(),
            "the durable record is still minted without a publisher"
        );
        Ok(())
    }
}
