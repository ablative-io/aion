//! gRPC-edge routing primitives: the directory-aware ownership guard for
//! signal/query/cancel (R-1/R-2) and the unsteered-start remint for `start`
//! (R-1).

use aion_core::WorkflowId;
use aion_store_haematite::HaematiteStore;

use super::directory::{NodeRef, OwnerView, ShardDirectory};

/// How many remint attempts per declared shard the unsteered-start loop is given
/// before falling back. Generous so that, even with a single owned shard out of
/// many, the probability of exhausting the budget without drawing an owned shard
/// is negligible, while still bounding the loop (§2.4 "bounded by shard count").
const REMINT_ATTEMPTS_PER_SHARD: usize = 16;

/// The routing verdict for a mutation/read (signal/query/cancel) whose target
/// `workflow_id` is known up front.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteDecision {
    /// This node owns the workflow's shard, owns all shards, is not clustered, or
    /// the owner is `Unknown` (route optimistically; the fence backstops):
    /// proceed to the local engine.
    Local,
    /// A live remote node owns the workflow's shard. R-2 cannot forward yet, so
    /// the edge returns `NotOwner` for this; R-3 forwards to `owner` when it has
    /// a `grpc_addr` and returns `NotOwner` only when it does not.
    Forward {
        /// The resolved remote owner (may or may not carry a `grpc_addr`).
        owner: NodeRef,
        /// The shard the workflow's durable state lives on (for the `NotOwner`
        /// fallback message and re-resolution).
        shard: usize,
    },
    /// Another node owns the workflow's shard and there is no forwarding target
    /// (no directory, or the owner declared no gRPC address). Return the typed
    /// retryable `NotOwner` carrying the shard so a routing-aware caller can
    /// re-resolve and retry.
    NotOwner {
        /// The distribution shard the workflow's durable state lives on.
        shard: usize,
    },
}

/// The placement decision for an unsteered `start` whose id has not been minted
/// yet (R-1 stopgap).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemintOutcome {
    /// Let the engine mint the id as usual: either there is no cluster, or this
    /// node owns every shard, so any minted id is already local. The default
    /// single-node path always takes this arm.
    EngineMint,
    /// Start with this pre-minted id, chosen so its shard is locally owned, so
    /// the start lands on this node and never fences.
    UseId(WorkflowId),
}

/// Route a signal/query/cancel at the edge through the shard directory.
///
/// `cluster_store`/`directory` are `None` for every single-node / non-clustered
/// boot — then the result is always [`RouteDecision::Local`] and the call is
/// byte-identical to today. With a cluster store and directory:
/// - the owner is this node, `Unknown`, or no directory → [`RouteDecision::Local`]
///   (own/optimistic; the fence backstops),
/// - a live remote owner → [`RouteDecision::Forward`] (R-3 forwards; R-2 maps it
///   to `NotOwner` since it has no forwarder yet).
#[must_use]
pub fn route_mutation(
    cluster_store: Option<&HaematiteStore>,
    directory: Option<&dyn ShardDirectory>,
    workflow_id: &WorkflowId,
) -> RouteDecision {
    let Some(store) = cluster_store else {
        return RouteDecision::Local;
    };
    let shard = store.shard_for_workflow(workflow_id);
    let Some(directory) = directory else {
        // Clustered but no directory wired: fall back to the bare local-ownership
        // check (R-1 behaviour) — own it or reject as NotOwner.
        return if store.owns_workflow_shard(workflow_id) {
            RouteDecision::Local
        } else {
            RouteDecision::NotOwner { shard }
        };
    };
    match directory.owner_of(shard) {
        OwnerView::Local | OwnerView::Unknown => RouteDecision::Local,
        OwnerView::Remote(owner) => RouteDecision::Forward { owner, shard },
    }
}

/// Decide an unsteered `start`'s placement at the edge (R-1, §2.4).
///
/// `cluster_store` is `None` for single-node / non-clustered boots → always
/// [`RemintOutcome::EngineMint`] (default path unchanged). With a cluster store
/// that owns only a subset of shards, returns [`RemintOutcome::UseId`] with an
/// id re-minted onto a locally-owned shard so the start never fences. An own-all
/// scope also yields `EngineMint` (any id is already local). On the (negligibly
/// likely) event the bounded remint loop is exhausted, falls back to
/// `EngineMint` rather than failing the start — the fence then backstops as it
/// did before routing existed.
#[must_use]
pub fn route_start(cluster_store: Option<&HaematiteStore>) -> RemintOutcome {
    let Some(store) = cluster_store else {
        return RemintOutcome::EngineMint;
    };
    let budget = store.shard_count().max(1) * REMINT_ATTEMPTS_PER_SHARD;
    match store.remint_for_owned_shard(budget) {
        Some(workflow_id) => RemintOutcome::UseId(workflow_id),
        None => RemintOutcome::EngineMint,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{RemintOutcome, RouteDecision, route_mutation, route_start};
    use aion_core::WorkflowId;
    use aion_store::StoreError;
    use aion_store_haematite::HaematiteStore;

    type TestResult = Result<(), StoreError>;

    fn unique_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "aion-routing-edge-{name}-{}-{nanos}-{counter}",
            std::process::id()
        ))
    }

    /// A multi-shard store owning exactly `owned` shards out of `shard_count`.
    fn store_owning(
        name: &str,
        shard_count: usize,
        owned: &[usize],
    ) -> Result<HaematiteStore, StoreError> {
        let store = HaematiteStore::create_with_shard_count(unique_dir(name), shard_count)?;
        store.set_owned_shards(owned.iter().copied());
        Ok(store)
    }

    /// No cluster store (single-node / non-clustered) always routes locally — the
    /// default path is a no-op.
    #[test]
    fn mutation_without_cluster_store_is_local() {
        let workflow_id = WorkflowId::new_v4();
        assert_eq!(
            route_mutation(None, None, &workflow_id),
            RouteDecision::Local
        );
    }

    /// Clustered but no directory wired (R-1 fallback): owned shard → `Local`,
    /// non-owned → `NotOwner`.
    #[test]
    fn mutation_without_directory_falls_back_to_bare_ownership() -> TestResult {
        let store = store_owning("mutation", 4, &[0])?;
        // Find one id whose shard is owned and one whose shard is not, so the
        // assertion exercises both arms regardless of hash distribution.
        let mut owned_id = None;
        let mut foreign_id = None;
        for _ in 0..10_000 {
            let candidate = WorkflowId::new_v4();
            if store.owns_workflow_shard(&candidate) {
                owned_id.get_or_insert(candidate);
            } else {
                foreign_id.get_or_insert(candidate);
            }
            if owned_id.is_some() && foreign_id.is_some() {
                break;
            }
        }
        let (Some(owned_id), Some(foreign_id)) = (owned_id, foreign_id) else {
            return Err(StoreError::Backend(
                "expected both an owned and a non-owned shard id".to_owned(),
            ));
        };

        assert_eq!(
            route_mutation(Some(&store), None, &owned_id),
            RouteDecision::Local
        );
        let shard = store.shard_for_workflow(&foreign_id);
        assert_eq!(
            route_mutation(Some(&store), None, &foreign_id),
            RouteDecision::NotOwner { shard }
        );
        Ok(())
    }

    /// With a directory, a non-owned shard whose owner is believed-down resolves
    /// `Unknown` → route locally (the fence backstops), not `NotOwner`.
    #[test]
    fn mutation_with_directory_routes_unknown_owner_locally() -> TestResult {
        use super::super::directory::{DirectoryPeer, StaticShardDirectory};
        let store = std::sync::Arc::new(store_owning("dir-unknown", 4, &[0])?);
        let directory = StaticShardDirectory::new(
            std::sync::Arc::clone(&store),
            vec![DirectoryPeer {
                name: "peer-1".to_owned(),
                owned_shards: vec![1, 2, 3],
                grpc_addr: None,
            }],
        );
        // A non-owned id: its shard's declared owner has no live link in this
        // single-node test store, so owner_of is Unknown → Local.
        let mut foreign_id = None;
        for _ in 0..10_000 {
            let candidate = WorkflowId::new_v4();
            if !store.owns_workflow_shard(&candidate) {
                foreign_id = Some(candidate);
                break;
            }
        }
        let Some(foreign_id) = foreign_id else {
            return Err(StoreError::Backend("expected a non-owned id".to_owned()));
        };
        assert_eq!(
            route_mutation(Some(store.as_ref()), Some(&directory), &foreign_id),
            RouteDecision::Local
        );
        Ok(())
    }

    /// No cluster store → engine mints the id (default path).
    #[test]
    fn start_without_cluster_store_uses_engine_mint() {
        assert_eq!(route_start(None), RemintOutcome::EngineMint);
    }

    /// Own-all scope (the single-node default after boot) → engine mints: any id
    /// is already local, so there is nothing to remint toward.
    #[test]
    fn start_with_own_all_scope_uses_engine_mint() -> TestResult {
        let store = HaematiteStore::create_with_shard_count(unique_dir("ownall"), 4)?;
        // No set_owned_shards call → owned_shards() == None == owns all.
        assert_eq!(route_start(Some(&store)), RemintOutcome::EngineMint);
        Ok(())
    }

    /// A subset-owning clustered node reminting a start always yields an id whose
    /// shard it owns, so the start never fences.
    #[test]
    fn start_reminted_id_lands_on_an_owned_shard() -> TestResult {
        let store = store_owning("remint", 4, &[1])?;
        let RemintOutcome::UseId(workflow_id) = route_start(Some(&store)) else {
            return Err(StoreError::Backend(
                "subset-owning node must remint, not engine-mint".to_owned(),
            ));
        };
        assert!(
            store.owns_workflow_shard(&workflow_id),
            "reminted id must land on an owned shard"
        );
        assert_eq!(store.shard_for_workflow(&workflow_id), 1);
        Ok(())
    }
}
