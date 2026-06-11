//! Consumption-gated wake confirmation for mailbox marker deliveries.
//!
//! beamr 0.4.9 has a lost-wakeup window in the scheduler's
//! `SliceOutcome::Wait` arm (`scheduler/execution/core.rs`): the post-store
//! mailbox re-check runs *before* the pid is inserted into the wait set, and
//! `wake_process` no-ops for pids absent from the wait set, so a marker
//! enqueued (and its wake issued) inside that gap leaves the process parked
//! with the message already in its mailbox. Nothing re-wakes it until the
//! next delivery — and a one-shot delivery (a release signal, a child
//! terminal) has no next delivery, so the workflow parks forever.
//!
//! The window is unreachable from inside beamr's public API (no call
//! synchronizes with the wait-set insert), but it is healable from outside:
//! a `wake_process` issued *after* the insert finds the pid in the wait set
//! and wakes it. The gap is only a few instructions wide, yet an OS-level
//! preemption can stretch it arbitrarily, so a fixed wake budget cannot be
//! correct. Instead every successful marker enqueue arms a wake ladder that
//! re-issues `wake_process` with exponential backoff (capped at the policy's
//! `ready_timeout`) until the *target proves it ran*: every suspending aion
//! native bumps its caller's wake-observation epoch on entry, and process
//! exit stamps a terminal epoch, so the ladder stops at the first observed
//! entry (or death) after the delivery. Surplus wakes are safe by
//! construction — every suspending native re-checks its recorded state on
//! re-entry and re-parks, and a plain BEAM `receive` re-checks its patterns.

use std::collections::BinaryHeap;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::config::SignalDeliveryConfig;
use crate::error::EngineError;

/// A scheduled follow-up wake for one delivered marker.
struct WakeOrder {
    /// When the next follow-up wake is due.
    due: Instant,
    /// Backoff that produced `due`; doubles up to the ladder cap.
    backoff: Duration,
    /// The pid-bound wake callback (`Scheduler::wake_notifier`).
    wake: Box<dyn Fn() + Send>,
    /// Stop condition: the target ran (or exited) after the delivery.
    done: Box<dyn Fn() -> bool + Send>,
}

impl PartialEq for WakeOrder {
    fn eq(&self, other: &Self) -> bool {
        self.due == other.due
    }
}

impl Eq for WakeOrder {}

impl PartialOrd for WakeOrder {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for WakeOrder {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // BinaryHeap is a max-heap; invert so the earliest due time wins.
        other.due.cmp(&self.due)
    }
}

/// Worker thread that confirms delivered markers were observed.
///
/// One thread per runtime: orders are queued over a channel and dispatched
/// from a due-time heap, so the delivery paths pay nothing beyond a channel
/// send. Shutdown drops the channel sender and joins the worker; pending
/// orders are discarded (the scheduler is stopping, so they are moot).
pub(super) struct WakeConfirmer {
    sender: Mutex<Option<Sender<WakeOrder>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    policy: SignalDeliveryConfig,
}

impl WakeConfirmer {
    /// Start the confirmation worker with the builder-supplied policy.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Runtime`] when the OS refuses the worker
    /// thread.
    pub(super) fn new(policy: SignalDeliveryConfig) -> Result<Self, EngineError> {
        let (sender, receiver) = channel();
        // The ladder cadence decays to the policy's readiness horizon: the
        // same bound the runtime applies to the inverse direction of the
        // spawn window. `max(initial)` keeps a degenerate zero policy from
        // busy-spinning the worker.
        let cap = policy.ready_timeout.max(policy.initial_backoff);
        let worker = std::thread::Builder::new()
            .name("aion-wake-confirm".to_owned())
            .spawn(move || run_worker(&receiver, cap))
            .map_err(|error| EngineError::Runtime {
                reason: format!("failed to start the wake-confirmation worker: {error}"),
            })?;
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            policy,
        })
    }

    /// Arm the wake ladder for one successfully enqueued marker.
    ///
    /// `wake` is the pid-bound `Scheduler::wake_notifier` callback; `done`
    /// reports whether the target has demonstrably run (or exited) since the
    /// delivery. A closed worker (shutdown in progress) drops the order
    /// silently — the scheduler is stopping and the wake is moot.
    pub(super) fn confirm(
        &self,
        wake: impl Fn() + Send + 'static,
        done: impl Fn() -> bool + Send + 'static,
    ) {
        let initial = self.policy.initial_backoff.max(Duration::from_micros(50));
        let order = WakeOrder {
            due: Instant::now() + initial,
            backoff: initial,
            wake: Box::new(wake),
            done: Box::new(done),
        };
        let guard = match self.sender.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(sender) = guard.as_ref() {
            // A send failure means the worker already exited (shutdown).
            drop(sender.send(order));
        }
    }

    /// Stop the worker: close the channel and join the thread.
    pub(super) fn shutdown(&self) {
        let sender = {
            let mut guard = match self.sender.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.take()
        };
        drop(sender);
        let worker = {
            let mut guard = match self.worker.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.take()
        };
        if let Some(worker) = worker
            && worker.join().is_err()
        {
            tracing::error!("wake-confirmation worker panicked");
        }
    }
}

impl Drop for WakeConfirmer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Dispatch loop: park until the earliest order is due or a new order
/// arrives, then settle every due order — finished ones are dropped, the
/// rest are woken and rescheduled with doubled backoff up to `cap`.
fn run_worker(receiver: &Receiver<WakeOrder>, cap: Duration) {
    let mut pending: BinaryHeap<WakeOrder> = BinaryHeap::new();
    loop {
        let arrival = match pending.peek() {
            Some(order) => {
                let wait = order.due.saturating_duration_since(Instant::now());
                match receiver.recv_timeout(wait) {
                    Ok(order) => Some(order),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            None => match receiver.recv() {
                Ok(order) => Some(order),
                Err(_disconnected) => return,
            },
        };
        if let Some(order) = arrival {
            pending.push(order);
        }
        let now = Instant::now();
        while pending.peek().is_some_and(|order| order.due <= now) {
            let Some(mut order) = pending.pop() else {
                break;
            };
            if (order.done)() {
                continue;
            }
            (order.wake)();
            let doubled = order.backoff.saturating_mul(2);
            order.backoff = if doubled > cap { cap } else { doubled };
            order.due = now + order.backoff;
            pending.push(order);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use super::WakeConfirmer;
    use crate::runtime::SignalDeliveryConfig;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn policy() -> SignalDeliveryConfig {
        SignalDeliveryConfig::new(
            Duration::from_millis(20),
            8,
            Duration::from_millis(1),
            Duration::from_millis(4),
        )
    }

    /// The ladder persists past any fixed budget until the target proves it
    /// ran — the lost-wakeup gap can be stretched arbitrarily by OS
    /// preemption, so a bounded ladder would strand the marker exactly when
    /// the machine is busiest. Before this contract a hung workflow
    /// reproduced at ~1/350 suite runs under saturation.
    #[test]
    fn ladder_persists_until_the_target_is_observed() -> TestResult {
        let confirmer = WakeConfirmer::new(policy())?;
        let wakes = Arc::new(AtomicU32::new(0));
        let observed = Arc::new(AtomicBool::new(false));
        let counter = Arc::clone(&wakes);
        let gate = Arc::clone(&observed);
        confirmer.confirm(
            move || {
                counter.fetch_add(1, Ordering::AcqRel);
            },
            move || gate.load(Ordering::Acquire),
        );

        // Well past the old 8-round budget: the ladder must keep waking.
        let deadline = Instant::now() + Duration::from_secs(10);
        while wakes.load(Ordering::Acquire) < 12 {
            if Instant::now() > deadline {
                return Err(format!(
                    "ladder stopped early at {} wakes without observation",
                    wakes.load(Ordering::Acquire)
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        // The target runs: the ladder stops at the next settle.
        observed.store(true, Ordering::Release);
        std::thread::sleep(Duration::from_millis(80));
        let settled = wakes.load(Ordering::Acquire);
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(
            wakes.load(Ordering::Acquire),
            settled,
            "no wakes may follow a positive observation"
        );
        confirmer.shutdown();
        Ok(())
    }

    /// An order whose target already ran issues no wake at all.
    #[test]
    fn already_observed_orders_never_wake() -> TestResult {
        let confirmer = WakeConfirmer::new(policy())?;
        let wakes = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&wakes);
        confirmer.confirm(
            move || {
                counter.fetch_add(1, Ordering::AcqRel);
            },
            || true,
        );
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(wakes.load(Ordering::Acquire), 0);
        confirmer.shutdown();
        Ok(())
    }

    /// Orders for several pids are interleaved on one worker, each stopping
    /// on its own observation.
    #[test]
    fn concurrent_orders_stop_independently() -> TestResult {
        let confirmer = WakeConfirmer::new(policy())?;
        let mut gates = Vec::new();
        let wakes = Arc::new(AtomicU32::new(0));
        for _ in 0..8 {
            let gate = Arc::new(AtomicBool::new(false));
            let counter = Arc::clone(&wakes);
            let observed = Arc::clone(&gate);
            confirmer.confirm(
                move || {
                    counter.fetch_add(1, Ordering::AcqRel);
                },
                move || observed.load(Ordering::Acquire),
            );
            gates.push(gate);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while wakes.load(Ordering::Acquire) < 16 {
            if Instant::now() > deadline {
                return Err("orders did not interleave".into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        for gate in &gates {
            gate.store(true, Ordering::Release);
        }
        std::thread::sleep(Duration::from_millis(80));
        let settled = wakes.load(Ordering::Acquire);
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(wakes.load(Ordering::Acquire), settled);
        confirmer.shutdown();
        Ok(())
    }

    /// Shutdown joins the worker and silently drops later orders.
    #[test]
    fn shutdown_is_idempotent_and_gates_new_orders() -> TestResult {
        let confirmer = WakeConfirmer::new(policy())?;
        confirmer.shutdown();
        confirmer.shutdown();
        let wakes = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&wakes);
        confirmer.confirm(
            move || {
                counter.fetch_add(1, Ordering::AcqRel);
            },
            || false,
        );
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            wakes.load(Ordering::Acquire),
            0,
            "orders after shutdown must be dropped"
        );
        Ok(())
    }
}
