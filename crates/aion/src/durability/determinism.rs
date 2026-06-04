//! Deterministic workflow-visible time and randomness.
//!
//! `DeterminismContext` is replay state, not a clock. Workflow-visible `now`
//! is always the timestamp recorded on the history event currently being
//! applied. Workflow-visible random values come from a fixed deterministic PRNG
//! seeded from the workflow/run identifiers. Recovery wall time, when supplied
//! elsewhere as an `as_of` value for expired-timer decisions, is intentionally
//! outside this context and is not workflow-visible.

use aion_core::{RunId, WorkflowId};
use chrono::{DateTime, Utc};
use rand_chacha::ChaCha20Rng;
use rand_core::{SeedableRng, TryRng};
use sha2::{Digest, Sha256};

/// Domain separator for deterministic workflow random seed derivation.
const RNG_SEED_DOMAIN: &[u8] = b"aion.durability.determinism.rng.v1.sha256.chacha20";

/// Per-execution deterministic state for workflow-visible time and randomness.
///
/// The current timestamp is advanced only from recorded event timestamps as
/// replay consumes history. Random output uses SHA-256 over a fixed domain plus
/// `WorkflowId` and `RunId` UUID bytes to seed `ChaCha20Rng`; no wall clock,
/// operating-system RNG, thread-local RNG, or other entropy source participates.
pub struct DeterminismContext {
    current_recorded_at: DateTime<Utc>,
    rng: ChaCha20Rng,
}

impl DeterminismContext {
    /// Creates deterministic state for a workflow run.
    ///
    /// `workflow_started_recorded_at` must be the `recorded_at` timestamp from
    /// the run's first recorded `WorkflowStarted` event. Before any later event
    /// is applied, [`Self::now`] returns this timestamp.
    #[must_use]
    pub fn new(
        workflow_started_recorded_at: DateTime<Utc>,
        workflow_id: &WorkflowId,
        run_id: &RunId,
    ) -> Self {
        Self {
            current_recorded_at: workflow_started_recorded_at,
            rng: ChaCha20Rng::from_seed(seed_from_ids(workflow_id, run_id)),
        }
    }

    /// Returns the currently applied recorded timestamp for workflow-visible
    /// `now`.
    #[must_use]
    pub const fn now(&self) -> DateTime<Utc> {
        self.current_recorded_at
    }

    /// Advances workflow-visible `now` to the timestamp of a newly applied
    /// recorded event.
    pub fn advance_to_recorded_at(&mut self, recorded_at: DateTime<Utc>) {
        self.current_recorded_at = recorded_at;
    }

    /// Draws the next deterministic workflow-visible random `u64`.
    ///
    /// The sequence is produced by `ChaCha20Rng` seeded with SHA-256 as
    /// documented on [`Self`], and is stable for the same `WorkflowId` + `RunId`
    /// across replays.
    #[must_use]
    pub fn next_random_u64(&mut self) -> u64 {
        match self.rng.try_next_u64() {
            Ok(value) => value,
        }
    }

    /// Fills bytes from the deterministic workflow-visible random stream.
    ///
    /// The bytes come from the same seeded `ChaCha20Rng` stream as
    /// [`Self::next_random_u64`].
    pub fn fill_random_bytes(&mut self, destination: &mut [u8]) {
        match self.rng.try_fill_bytes(destination) {
            Ok(()) => (),
        }
    }
}

/// Derives the fixed-size `ChaCha20` seed from workflow/run identifiers.
fn seed_from_ids(workflow_id: &WorkflowId, run_id: &RunId) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(RNG_SEED_DOMAIN);
    hasher.update(workflow_id.as_uuid().as_bytes());
    hasher.update(run_id.as_uuid().as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use aion_core::{RunId, WorkflowId};
    use chrono::{DateTime, TimeZone, Utc};
    use uuid::Uuid;

    use super::DeterminismContext;

    fn timestamp(seconds: i64) -> Result<DateTime<Utc>, Box<dyn std::error::Error>> {
        Utc.timestamp_opt(seconds, 0)
            .single()
            .ok_or_else(|| format!("invalid fixed timestamp {seconds}").into())
    }

    fn workflow_id() -> WorkflowId {
        WorkflowId::new(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888))
    }

    fn run_id(value: u128) -> RunId {
        RunId::new(Uuid::from_u128(value))
    }

    fn random_sequence(context: &mut DeterminismContext) -> Vec<u64> {
        (0..16).map(|_| context.next_random_u64()).collect()
    }

    #[test]
    fn now_starts_at_workflow_started_and_advances_with_recorded_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let started_at = timestamp(1_700_000_000)?;
        let first_event_at = timestamp(1_700_000_010)?;
        let second_event_at = timestamp(1_700_000_020)?;
        let mut context = DeterminismContext::new(
            started_at,
            &workflow_id(),
            &run_id(0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0000),
        );

        assert_eq!(context.now(), started_at);
        context.advance_to_recorded_at(first_event_at);
        assert_eq!(context.now(), first_event_at);
        context.advance_to_recorded_at(second_event_at);
        assert_eq!(context.now(), second_event_at);

        Ok(())
    }

    #[test]
    fn identical_recorded_sequences_have_identical_now_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let started_at = timestamp(1_700_100_000)?;
        let events = [
            timestamp(1_700_100_001)?,
            timestamp(1_700_100_005)?,
            timestamp(1_700_100_030)?,
        ];
        let workflow_id = workflow_id();
        let run_id = run_id(0xaaaa_bbbb_cccc_dddd_eeee_ffff_0000_1111);
        let mut first = DeterminismContext::new(started_at, &workflow_id, &run_id);
        let mut second = DeterminismContext::new(started_at, &workflow_id, &run_id);

        assert_eq!(first.now(), second.now());
        for recorded_at in events {
            first.advance_to_recorded_at(recorded_at);
            second.advance_to_recorded_at(recorded_at);
            assert_eq!(first.now(), second.now());
        }

        Ok(())
    }

    #[test]
    fn same_workflow_and_run_produce_identical_random_sequence()
    -> Result<(), Box<dyn std::error::Error>> {
        let started_at = timestamp(1_700_200_000)?;
        let workflow_id = workflow_id();
        let run_id = run_id(0xbbbb_cccc_dddd_eeee_ffff_0000_1111_2222);
        let mut first = DeterminismContext::new(started_at, &workflow_id, &run_id);
        let mut second = DeterminismContext::new(started_at, &workflow_id, &run_id);

        assert_eq!(random_sequence(&mut first), random_sequence(&mut second));

        Ok(())
    }

    #[test]
    fn different_run_ids_produce_different_random_sequences()
    -> Result<(), Box<dyn std::error::Error>> {
        let started_at = timestamp(1_700_300_000)?;
        let workflow_id = workflow_id();
        let first_run_id = run_id(0xcccc_dddd_eeee_ffff_0000_1111_2222_3333);
        let second_run_id = run_id(0xdddd_eeee_ffff_0000_1111_2222_3333_4444);
        let mut first = DeterminismContext::new(started_at, &workflow_id, &first_run_id);
        let mut second = DeterminismContext::new(started_at, &workflow_id, &second_run_id);

        assert_ne!(random_sequence(&mut first), random_sequence(&mut second));

        Ok(())
    }

    #[test]
    fn deterministic_random_bytes_are_replay_identical() -> Result<(), Box<dyn std::error::Error>> {
        let started_at = timestamp(1_700_400_000)?;
        let workflow_id = workflow_id();
        let run_id = run_id(0xeeee_ffff_0000_1111_2222_3333_4444_5555);
        let mut first = DeterminismContext::new(started_at, &workflow_id, &run_id);
        let mut second = DeterminismContext::new(started_at, &workflow_id, &run_id);
        let mut first_bytes = [0_u8; 64];
        let mut second_bytes = [0_u8; 64];

        first.fill_random_bytes(&mut first_bytes);
        second.fill_random_bytes(&mut second_bytes);

        assert_eq!(first_bytes, second_bytes);

        Ok(())
    }
}
