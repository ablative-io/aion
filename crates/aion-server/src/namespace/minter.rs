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

use aion_store::{MintOutcome, NamespaceOrigin, NamespaceStore};

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
}

impl std::fmt::Debug for NamespaceMinter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NamespaceMinter")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl NamespaceMinter {
    /// Build a minter over a durable namespace store and an auto-create policy.
    #[must_use]
    pub fn new(store: Arc<dyn NamespaceStore>, policy: AutoCreate) -> Self {
        Self { store, policy }
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
                        tracing::info!(
                            namespace = %namespace,
                            origin = origin_label(origin),
                            "namespace created"
                        );
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
