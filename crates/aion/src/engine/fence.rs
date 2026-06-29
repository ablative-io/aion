//! The double-adoption fence-ordering planner (ADR-021 clean-partial).
//!
//! When a survivor adopts a dead peer's shards it must NOT let a shard it lost
//! the fence on still widen its enumeration scope or recover (re-execute) that
//! shard's workflows. Two survivors racing adoption of the same shard would
//! otherwise BOTH execute its workflows — the double-execution hazard this module
//! closes.
//!
//! The fix is an ORDERING INVARIANT, per shard: the publish-fence happens BEFORE
//! the shard contributes to the widened owned-shards scope AND before it is
//! recovered. [`plan_adopted_shards`] is the pure realization of that invariant —
//! extracted from `Engine::adopt_shards_inner` so the ordering can be tested
//! directly, including a NEGATIVE CONTROL ([`plan_adopted_shards_prefix_buggy`])
//! that uses the pre-fix order and is proven to double-execute.

use aion_store::EventStore;
use aion_store::StoreError;

/// The per-shard fence operations the adoption planner drives, abstracted so the
/// ordering invariant is testable without a full distributed store. In production
/// this is implemented over `dyn ReadableEventStore` (see
/// [`StoreFenceSeam`]); tests implement a recording fake.
pub(super) trait ShardFenceSeam {
    /// Win the per-shard election and become the fenced live owner of `shard`
    /// (union-merging its committed history locally). `Err(NotOwner)` = a clean,
    /// droppable election loss; any other error aborts adoption.
    fn acquire(&self, shard: usize) -> Result<(), StoreError>;

    /// Publish this node as `shard`'s current owner in the cluster directory,
    /// fenced by the election just won. `Err(NotOwner)` = deposed in the residual
    /// window (drop the shard); any other error aborts adoption.
    fn publish(&self, shard: usize) -> Result<(), StoreError>;

    /// Whether this node still holds live serve-authority for `shard` — the
    /// residual-window re-assertion that excludes a shard deposed after publish.
    fn is_current_owner(&self, shard: usize) -> bool;

    /// Widen the owned-enumeration scope by EXACTLY `shards` (the survivors), ONCE.
    /// Recovery enumerates over this union, so a dropped shard widens nothing.
    fn extend(&self, shards: &[usize]);
}

/// The production [`ShardFenceSeam`], forwarding each operation to the store's
/// type-erased `dyn ReadableEventStore` (`EventStore: ReadableEventStore`). A
/// single-node / non-distributed store no-ops every method, so the planner over
/// it is byte-identical to the pre-fix behaviour (acquire/publish Ok, extend
/// no-op, `is_current_owner` true).
pub(super) struct StoreFenceSeam<'a> {
    pub store: &'a dyn EventStore,
}

impl ShardFenceSeam for StoreFenceSeam<'_> {
    fn acquire(&self, shard: usize) -> Result<(), StoreError> {
        self.store.acquire_owned_shard(shard)
    }
    fn publish(&self, shard: usize) -> Result<(), StoreError> {
        self.store.publish_shard_owner(shard)
    }
    fn is_current_owner(&self, shard: usize) -> bool {
        self.store.is_current_owner(shard)
    }
    fn extend(&self, shards: &[usize]) {
        self.store.extend_owned_shards(shards);
    }
}

/// Drive the fence in the FIXED order and return the set of shards that survived
/// it — the shards recovery may safely re-execute (ADR-021 clean-partial).
///
/// Order, per shard: `acquire` → `publish` as a UNIT; a `NotOwner` from EITHER
/// step drops the shard (it never reaches `extend`, is never returned, and is
/// never a hard error). After the loop the survivors are re-asserted with
/// `is_current_owner` (excluding any deposed in the residual window) and
/// `extend`ed ONCE. The publish-fence is therefore guaranteed to precede BOTH the
/// scope widening AND (downstream) recovery, per shard.
///
/// # Errors
///
/// Propagates any non-`NotOwner` store error from `acquire` / `publish` (a real
/// backend/quorum failure aborts the whole adoption); a `NotOwner` is never an
/// error — the shard is silently dropped.
pub(super) fn plan_adopted_shards<S: ShardFenceSeam>(
    seam: &S,
    shards: &[usize],
) -> Result<Vec<usize>, StoreError> {
    let mut committed: Vec<usize> = Vec::with_capacity(shards.len());
    for &shard in shards {
        // acquire → publish as a UNIT. A `NotOwner` from either step (a deposed
        // survivor) drops the shard; only a shard that survived BOTH is committed.
        if fence_survives(seam, shard)? {
            committed.push(shard);
        }
    }
    // Re-assert live ownership; exclude any shard deposed in the residual window
    // between its publish and now.
    let recoverable: Vec<usize> = committed
        .into_iter()
        .filter(|&shard| seam.is_current_owner(shard))
        .collect();
    // Widen the scope ONCE, over exactly the survivors. A deposed survivor
    // contributed nothing — zero widened scope, nothing to recover.
    seam.extend(&recoverable);
    Ok(recoverable)
}

/// Drive `acquire` then `publish` for ONE shard as a unit (PUBLISH-FENCE BEFORE
/// any caller widens scope). Returns `Ok(true)` when the shard survived BOTH,
/// `Ok(false)` when it was cleanly fenced (`NotOwner`) at either step, and an
/// error only for a real non-`NotOwner` store failure.
fn fence_survives<S: ShardFenceSeam>(seam: &S, shard: usize) -> Result<bool, StoreError> {
    match seam.acquire(shard) {
        Ok(()) => {}
        Err(StoreError::NotOwner { .. }) => return Ok(false),
        Err(error) => return Err(error),
    }
    match seam.publish(shard) {
        Ok(()) => Ok(true),
        Err(StoreError::NotOwner { .. }) => Ok(false),
        Err(error) => Err(error),
    }
}

/// NEGATIVE CONTROL — the PRE-FIX (buggy) order: `extend` ALL acquired shards
/// BEFORE publishing, then publish. A survivor that won the election but is
/// deposed at publish-time has ALREADY widened its scope (and would recover the
/// shard), so two survivors both execute its workflows. Exists ONLY to prove the
/// falsifiability test detects the bug; never called by production.
#[cfg(test)]
pub(super) fn plan_adopted_shards_prefix_buggy<S: ShardFenceSeam>(
    seam: &S,
    shards: &[usize],
) -> Result<Vec<usize>, StoreError> {
    // Pre-fix order: acquire every shard, then EXTEND before any publish-fence.
    let mut acquired: Vec<usize> = Vec::with_capacity(shards.len());
    for &shard in shards {
        match seam.acquire(shard) {
            Ok(()) => acquired.push(shard),
            Err(StoreError::NotOwner { .. }) => {}
            Err(error) => return Err(error),
        }
    }
    // BUG: widen scope (→ recovery enumerates these) BEFORE the publish-fence.
    seam.extend(&acquired);
    // Publish afterwards; a fenced shard is already in the recoverable scope.
    for &shard in &acquired {
        match seam.publish(shard) {
            Ok(()) | Err(StoreError::NotOwner { .. }) => {}
            Err(error) => return Err(error),
        }
    }
    // The buggy order recovers everything it acquired, fence be damned.
    Ok(acquired)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    use super::{
        ShardFenceSeam, StoreError, plan_adopted_shards, plan_adopted_shards_prefix_buggy,
    };

    /// One recorded fence call, in invocation order, so a test can assert that
    /// `extend` (and therefore recovery) never precedes a shard's publish-fence.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Acquire(usize),
        Publish(usize),
        IsOwner(usize),
        Extend(Vec<usize>),
    }

    /// A controllable fence seam: each shard can be made to lose at `acquire` or
    /// `publish` (returning `NotOwner`), or to fail the residual `is_current_owner`
    /// re-assertion. Records every call in order. The `executed` counter models the
    /// EXTERNAL side effect of recovery: it is bumped once per shard that the
    /// planner declares recoverable AND that the (shared) world has not yet seen —
    /// i.e. the observable single-vs-double execution count.
    struct FakeSeam<'a> {
        lose_acquire: BTreeSet<usize>,
        lose_publish: BTreeSet<usize>,
        lose_residual: BTreeSet<usize>,
        calls: RefCell<Vec<Call>>,
        /// Shared across survivors: the set of shards already executed in the world.
        executed_world: &'a RefCell<BTreeSet<usize>>,
        /// External executions THIS planner run drove (the side-effect count).
        executed_here: RefCell<usize>,
    }

    impl<'a> FakeSeam<'a> {
        fn new(executed_world: &'a RefCell<BTreeSet<usize>>) -> Self {
            Self {
                lose_acquire: BTreeSet::new(),
                lose_publish: BTreeSet::new(),
                lose_residual: BTreeSet::new(),
                calls: RefCell::new(Vec::new()),
                executed_world,
                executed_here: RefCell::new(0),
            }
        }
        fn lose_acquire(mut self, shard: usize) -> Self {
            self.lose_acquire.insert(shard);
            self
        }
        fn lose_publish(mut self, shard: usize) -> Self {
            self.lose_publish.insert(shard);
            self
        }
        fn lose_residual(mut self, shard: usize) -> Self {
            self.lose_residual.insert(shard);
            self
        }
        /// Model recovery: execute the EXTERNAL side effect once per recoverable
        /// shard. Counts a duplicate against the shared world so a second survivor
        /// adopting the same shard is detectable as a double execution.
        fn recover(&self, recoverable: &[usize]) {
            for &shard in recoverable {
                *self.executed_here.borrow_mut() += 1;
                self.executed_world.borrow_mut().insert(shard);
            }
        }
        fn calls(&self) -> Vec<Call> {
            self.calls.borrow().clone()
        }
    }

    impl ShardFenceSeam for FakeSeam<'_> {
        fn acquire(&self, shard: usize) -> Result<(), StoreError> {
            self.calls.borrow_mut().push(Call::Acquire(shard));
            if self.lose_acquire.contains(&shard) {
                return Err(StoreError::NotOwner { shard });
            }
            Ok(())
        }
        fn publish(&self, shard: usize) -> Result<(), StoreError> {
            self.calls.borrow_mut().push(Call::Publish(shard));
            if self.lose_publish.contains(&shard) {
                return Err(StoreError::NotOwner { shard });
            }
            Ok(())
        }
        fn is_current_owner(&self, shard: usize) -> bool {
            self.calls.borrow_mut().push(Call::IsOwner(shard));
            !self.lose_residual.contains(&shard)
        }
        fn extend(&self, shards: &[usize]) {
            self.calls.borrow_mut().push(Call::Extend(shards.to_vec()));
        }
    }

    /// Index of the first `Extend` call (recovery's gate), or `usize::MAX`.
    fn first_extend_index(calls: &[Call]) -> usize {
        calls
            .iter()
            .position(|call| matches!(call, Call::Extend(_)))
            .unwrap_or(usize::MAX)
    }

    /// Index of `shard`'s publish, or `usize::MAX` if never published.
    fn publish_index(calls: &[Call], shard: usize) -> usize {
        calls
            .iter()
            .position(|call| matches!(call, Call::Publish(s) if *s == shard))
            .unwrap_or(usize::MAX)
    }

    /// ORDERING INVARIANT: for every survivor, its publish precedes the single
    /// `extend` (and therefore recovery). The fix.
    #[test]
    fn publish_fence_precedes_extend_for_every_survivor() -> Result<(), StoreError> {
        let world = RefCell::new(BTreeSet::new());
        let seam = FakeSeam::new(&world);
        let recoverable = plan_adopted_shards(&seam, &[0, 1, 2])?;
        assert_eq!(recoverable, vec![0, 1, 2]);
        let calls = seam.calls();
        let extend_at = first_extend_index(&calls);
        for shard in &recoverable {
            assert!(
                publish_index(&calls, *shard) < extend_at,
                "shard {shard}'s publish-fence must precede the scope widening (extend)"
            );
        }
        // Exactly one extend, carrying exactly the survivors.
        assert_eq!(
            calls.iter().filter(|c| matches!(c, Call::Extend(_))).count(),
            1,
            "extend runs exactly once"
        );
        assert!(calls.contains(&Call::Extend(vec![0, 1, 2])));
        Ok(())
    }

    /// DEPOSED SURVIVOR (publish-fenced): a shard that loses the fence at publish
    /// is NEVER extended and NEVER recovered — zero widened scope, zero external
    /// executions — and it is NOT a hard error.
    #[test]
    fn deposed_at_publish_leaves_scope_unchanged_and_recovers_nothing() -> Result<(), StoreError> {
        let world = RefCell::new(BTreeSet::new());
        let seam = FakeSeam::new(&world).lose_publish(7);
        let recoverable = plan_adopted_shards(&seam, &[7])?;
        assert!(recoverable.is_empty(), "a deposed shard is not recoverable");
        seam.recover(&recoverable);
        assert_eq!(*seam.executed_here.borrow(), 0, "zero external executions");
        // The single extend carried an EMPTY survivor set: no widened scope.
        assert!(
            seam.calls().contains(&Call::Extend(vec![])),
            "extend runs once over an empty survivor set — owned scope unchanged"
        );
        Ok(())
    }

    /// DEPOSED SURVIVOR (acquire-fenced): same drop at the earlier step.
    #[test]
    fn deposed_at_acquire_recovers_nothing() -> Result<(), StoreError> {
        let world = RefCell::new(BTreeSet::new());
        let seam = FakeSeam::new(&world).lose_acquire(3);
        let recoverable = plan_adopted_shards(&seam, &[3])?;
        assert!(recoverable.is_empty());
        // A losing acquire must NOT even attempt a publish for that shard.
        assert!(
            !seam.calls().iter().any(|c| matches!(c, Call::Publish(3))),
            "a shard that lost acquire is never published"
        );
        Ok(())
    }

    /// PARTIAL WIN A / FENCED B: A survives both steps (extended + recoverable),
    /// B loses the publish fence (absent from scope, never recovered).
    #[test]
    fn partial_win_a_fenced_b() -> Result<(), StoreError> {
        let world = RefCell::new(BTreeSet::new());
        let seam = FakeSeam::new(&world).lose_publish(1);
        let recoverable = plan_adopted_shards(&seam, &[0, 1])?;
        assert_eq!(recoverable, vec![0], "only A survives the fence");
        seam.recover(&recoverable);
        assert_eq!(*seam.executed_here.borrow(), 1, "A executes exactly once");
        assert!(seam.calls().contains(&Call::Extend(vec![0])));
        assert!(
            !world.borrow().contains(&1),
            "B is never executed (never recovered)"
        );
        Ok(())
    }

    /// RESIDUAL-WINDOW DEPOSE: a shard that publishes successfully but then fails
    /// the `is_current_owner` re-assertion is excluded from recovery.
    #[test]
    fn residual_window_depose_excludes_shard() -> Result<(), StoreError> {
        let world = RefCell::new(BTreeSet::new());
        let seam = FakeSeam::new(&world).lose_residual(5);
        let recoverable = plan_adopted_shards(&seam, &[5])?;
        assert!(
            recoverable.is_empty(),
            "a shard deposed in the residual window is not recovered"
        );
        Ok(())
    }

    /// FALSIFIABILITY CONTROL (the crown jewel). Two survivors race adoption of
    /// the SAME shard. The first wins the fence; the second is deposed at publish.
    /// Under the FIX, the shared external-execution count is EXACTLY 1. Under the
    /// PRE-FIX order (extend-before-publish) the deposed second survivor still
    /// recovers the shard, so the count is 2 — proving the test detects the bug.
    #[test]
    fn falsifiability_external_execution_is_exactly_once_under_fix() -> Result<(), StoreError> {
        const SHARD: usize = 0;

        // ---- FIX ORDER: winner publishes; loser is fenced at publish. ----
        let world = RefCell::new(BTreeSet::new());
        // Survivor 1 wins the fence and executes.
        let winner = FakeSeam::new(&world);
        let won = plan_adopted_shards(&winner, &[SHARD])?;
        winner.recover(&won);
        // Survivor 2 is deposed at publish (the winner already owns it).
        let loser = FakeSeam::new(&world).lose_publish(SHARD);
        let lost = plan_adopted_shards(&loser, &[SHARD])?;
        loser.recover(&lost);

        let fixed_total = *winner.executed_here.borrow() + *loser.executed_here.borrow();
        assert_eq!(
            fixed_total, 1,
            "under the fence fix the shard's workflow executes EXACTLY once across both survivors"
        );

        // ---- NEGATIVE CONTROL: the SAME race against the pre-fix order. ----
        let world_buggy = RefCell::new(BTreeSet::new());
        let winner_b = FakeSeam::new(&world_buggy);
        let won_b = plan_adopted_shards_prefix_buggy(&winner_b, &[SHARD])?;
        winner_b.recover(&won_b);
        // The deposed second survivor STILL extended before its publish-fence, so
        // the buggy planner returns the shard as recoverable and it re-executes.
        let loser_b = FakeSeam::new(&world_buggy).lose_publish(SHARD);
        let lost_b = plan_adopted_shards_prefix_buggy(&loser_b, &[SHARD])?;
        loser_b.recover(&lost_b);

        let buggy_total = *winner_b.executed_here.borrow() + *loser_b.executed_here.borrow();
        assert_eq!(
            buggy_total, 2,
            "the PRE-FIX order double-executes the shard's workflow — this is the bug the fix \
             closes, and proves this falsifiability test actually detects it"
        );
        Ok(())
    }
}
