//! Candidate-cycling redial driver for the liminal worker transport (G-1, #112).
//!
//! # Why this exists
//!
//! A [`LiminalActivityWorker`](crate::LiminalActivityWorker) dials ONE liminal
//! listen address and its serve loop returns on the first transport error. Each
//! deployed `aion server` hosts its OWN liminal listener backed by its OWN
//! per-process connected-worker registry, so when the owner of a shard is
//! `kill -9`'d the worker's connection drops and it cannot migrate to the
//! survivor that adopts the shard — the survivor listens on a DIFFERENT address
//! and has a DISTINCT registry. The gRPC outbox transport already survives this
//! (each worker dials per-endpoint and redials with a high attempt budget); this
//! module gives the liminal-push path the same survival.
//!
//! # The design (D-1: static candidate-address list)
//!
//! The worker is handed a STATIC list of candidate addresses (every server's
//! `liminal_listen_address`). The redial driver here owns the orchestration: it
//! connects to a candidate, serves until the connection drops, and on drop
//! ADVANCES to the next candidate (wrapping) and reconnects, with bounded
//! exponential backoff that resets after a connection that actually served.
//! Reconnecting re-runs `connect_with_registration`, so the worker re-registers
//! in the SURVIVOR's registry and becomes selectable there — idempotently,
//! because the survivor never saw the worker's prior (owner) connection.
//!
//! # Why a generic, transport-free core
//!
//! The cycling + backoff logic is expressed over two injected closures
//! (`connect` and `serve`) and a [`CandidateCursor`], with no socket, no liminal
//! types, and no async. That keeps it deterministic and unit/mutation-testable
//! without spawning servers: a test drives the exact candidate-advance and
//! backoff-reset behaviour with in-memory fakes. [`LiminalActivityWorker`] wires
//! the real `connect`/`serve` closures into [`run_redial_loop`].

use std::time::Duration;

/// Bounded exponential-backoff schedule for reconnect attempts.
///
/// `current` starts at `initial` and is multiplied by 2 after each failed
/// connect (saturating at `max`); a connection that served at least one unit of
/// work resets it back to `initial` (a transient blip should not inflate the
/// pause before the next reconnect).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RedialBackoff {
    initial: Duration,
    max: Duration,
    current: Duration,
}

impl RedialBackoff {
    /// Builds a backoff schedule starting at `initial`, capped at `max`.
    ///
    /// `max` is clamped up to `initial` so the schedule is always well-formed
    /// even if a caller passes `max < initial`.
    #[must_use]
    pub fn new(initial: Duration, max: Duration) -> Self {
        let max = max.max(initial);
        Self {
            initial,
            max,
            current: initial,
        }
    }

    /// The pause to wait before the next reconnect attempt.
    #[must_use]
    pub const fn current(&self) -> Duration {
        self.current
    }

    /// Doubles the pause for the next attempt, saturating at `max`.
    pub fn increase(&mut self) {
        let doubled = self.current.saturating_mul(2);
        self.current = doubled.min(self.max);
    }

    /// Resets the pause back to `initial` after a connection that served work.
    pub fn reset(&mut self) {
        self.current = self.initial;
    }
}

/// A non-empty ring of candidate addresses with a wrapping cursor.
///
/// The cursor names the candidate the next connect attempt should dial.
/// [`CandidateCursor::advance`] moves it to the next candidate (wrapping back to
/// the first after the last), which is what lets the worker migrate from a dead
/// owner to a live survivor.
#[derive(Clone, Debug)]
pub struct CandidateCursor {
    candidates: Vec<String>,
    index: usize,
}

impl CandidateCursor {
    /// Builds a cursor over `candidates`, starting at the first.
    ///
    /// # Errors
    ///
    /// Returns [`RedialError::NoCandidates`] when the list is empty — a worker
    /// with no address to dial can never serve, and an empty ring would make
    /// [`CandidateCursor::current`] and [`CandidateCursor::advance`] meaningless.
    pub fn new(candidates: Vec<String>) -> Result<Self, RedialError> {
        if candidates.is_empty() {
            return Err(RedialError::NoCandidates);
        }
        Ok(Self {
            candidates,
            index: 0,
        })
    }

    /// The address the next connect attempt should dial.
    ///
    /// The constructor rejects an empty ring and [`CandidateCursor::advance`]
    /// keeps `index` in range via modulo, so `index` always names a present
    /// element; the `first()` fallback exists only to keep this total without a
    /// panic path, and is never reached in practice.
    #[must_use]
    pub fn current(&self) -> &str {
        self.candidates
            .get(self.index)
            .or_else(|| self.candidates.first())
            .map_or("", String::as_str)
    }

    /// Advances to the next candidate, wrapping after the last.
    pub fn advance(&mut self) {
        self.index = (self.index + 1) % self.candidates.len();
    }
}

/// Errors the redial driver surfaces before or during the cycle.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum RedialError {
    /// The candidate list was empty, so there is nothing to dial.
    #[error("liminal worker redial requires at least one candidate address")]
    NoCandidates,
}

/// Outcome of one served connection, telling the loop how to proceed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeResult {
    /// The caller's stop condition fired; the loop should exit cleanly.
    Stopped,
    /// The connection dropped (transport error); the loop should redial the next
    /// candidate. `served_work` distinguishes a connection that did useful work
    /// (reset backoff) from one that dropped without serving (keep backing off).
    Dropped {
        /// Whether this connection served at least one dispatch before dropping.
        served_work: bool,
    },
}

/// Drives the candidate-cycling redial loop until the served connection reports
/// [`ServeResult::Stopped`] or a connect attempt fails non-retryably.
///
/// `connect` dials the cursor's current candidate, returning a live connection
/// handle or an error. `serve` runs the serve loop over a connection until it
/// stops or drops. `sleep` pauses for the backoff duration (injected so tests
/// observe the schedule without real time). `stop` is consulted after every
/// connect failure so a shutdown is honoured even while backing off.
///
/// On a served connection that DROPPED, the loop ADVANCES the cursor (migrating
/// to the next candidate) and reconnects. On a CONNECT failure it also advances
/// (the dialed candidate may be the dead owner) and backs off. The single
/// load-bearing migration step is `cursor.advance()` — break it and a worker can
/// never leave a dead owner.
///
/// # Errors
///
/// Returns the connect closure's error type only when `connect` reports a
/// non-retryable failure (`is_retryable(&err) == false`); retryable connect
/// failures are absorbed into the backoff cycle and never surfaced.
pub fn run_redial_loop<Conn, Connect, Serve, Sleep, Stop, IsRetryable, Err>(
    cursor: &mut CandidateCursor,
    backoff: &mut RedialBackoff,
    mut connect: Connect,
    mut serve: Serve,
    mut sleep: Sleep,
    mut stop: Stop,
    is_retryable: IsRetryable,
) -> Result<(), Err>
where
    Connect: FnMut(&str) -> Result<Conn, Err>,
    Serve: FnMut(Conn) -> ServeResult,
    Sleep: FnMut(Duration),
    Stop: FnMut() -> bool,
    IsRetryable: Fn(&Err) -> bool,
{
    loop {
        if stop() {
            return Ok(());
        }
        match connect(cursor.current()) {
            Ok(connection) => match serve(connection) {
                ServeResult::Stopped => return Ok(()),
                ServeResult::Dropped { served_work } => {
                    if served_work {
                        backoff.reset();
                    }
                    // Migrate to the next candidate on every drop so a dead
                    // owner is abandoned for the survivor that adopted its shard.
                    cursor.advance();
                }
            },
            Err(error) => {
                if !is_retryable(&error) {
                    return Err(error);
                }
                // The dialed candidate is unreachable (the likely-dead owner, or
                // a survivor whose listener is not up yet): advance and back off
                // so the next attempt tries the next candidate after a pause.
                cursor.advance();
                sleep(backoff.current());
                backoff.increase();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::Duration;

    use super::{CandidateCursor, RedialBackoff, RedialError, ServeResult, run_redial_loop};

    fn cursor(addresses: &[&str]) -> Result<CandidateCursor, RedialError> {
        CandidateCursor::new(
            addresses
                .iter()
                .map(|address| (*address).to_owned())
                .collect(),
        )
    }

    #[test]
    fn empty_candidate_list_is_rejected() {
        let error = CandidateCursor::new(Vec::new()).err();
        assert_eq!(error, Some(RedialError::NoCandidates));
    }

    #[test]
    fn cursor_advances_and_wraps() -> Result<(), RedialError> {
        let mut ring = cursor(&["a", "b", "c"])?;
        assert_eq!(ring.current(), "a");
        ring.advance();
        assert_eq!(ring.current(), "b");
        ring.advance();
        assert_eq!(ring.current(), "c");
        ring.advance();
        assert_eq!(
            ring.current(),
            "a",
            "advancing past the last wraps to the first"
        );
        Ok(())
    }

    #[test]
    fn backoff_grows_then_resets() {
        let mut backoff = RedialBackoff::new(Duration::from_millis(10), Duration::from_millis(40));
        assert_eq!(backoff.current(), Duration::from_millis(10));
        backoff.increase();
        assert_eq!(backoff.current(), Duration::from_millis(20));
        backoff.increase();
        assert_eq!(backoff.current(), Duration::from_millis(40));
        backoff.increase();
        assert_eq!(
            backoff.current(),
            Duration::from_millis(40),
            "saturates at max"
        );
        backoff.reset();
        assert_eq!(
            backoff.current(),
            Duration::from_millis(10),
            "served work resets the schedule"
        );
    }

    #[test]
    fn backoff_max_is_clamped_up_to_initial() {
        let backoff = RedialBackoff::new(Duration::from_millis(50), Duration::from_millis(10));
        assert_eq!(backoff.current(), Duration::from_millis(50));
    }

    /// THE GATE TEST (mutation target): when the connection to the OWNER drops,
    /// the loop must MIGRATE to the survivor candidate via `cursor.advance()` and
    /// re-connect (re-register) there. We drive the loop with in-memory fakes:
    /// the owner connection drops on first serve; the survivor connection then
    /// serves and stops. The assertion is that the survivor was dialed — which is
    /// ONLY true if the cursor advanced. Breaking `advance()` (the mutation)
    /// makes the loop re-dial the owner forever and never reach the survivor, so
    /// the recorded dial sequence never contains the survivor and this fails.
    #[test]
    fn redial_migrates_to_survivor_on_owner_drop() -> Result<(), RedialError> {
        let owner = "owner:1";
        let survivor = "survivor:2";
        let mut ring = cursor(&[owner, survivor])?;
        let mut backoff = RedialBackoff::new(Duration::from_millis(1), Duration::from_millis(4));

        let dialed: RefCell<Vec<String>> = RefCell::new(Vec::new());
        // Each connect succeeds (the listener is reachable); the serve outcome is
        // what differs: the owner's connection drops, the survivor's stops.
        let connect = |address: &str| -> Result<String, &'static str> {
            dialed.borrow_mut().push(address.to_owned());
            Ok(address.to_owned())
        };
        let serve = |connection: String| {
            if connection == owner {
                ServeResult::Dropped { served_work: true }
            } else {
                ServeResult::Stopped
            }
        };
        // A bound so a BROKEN advance (the mutation: cursor never leaves the
        // owner) terminates deterministically with a failed assertion rather than
        // hanging — after this many owner dials the stop fires and the survivor is
        // proven absent. With a correct advance the survivor is dialed 2nd and the
        // loop stops there long before the bound.
        let dial_cap = 8usize;
        let stop = || dialed.borrow().len() >= dial_cap;

        let result = run_redial_loop(
            &mut ring,
            &mut backoff,
            connect,
            serve,
            |_| {},
            stop,
            |_err| true,
        );
        assert_eq!(result, Ok(()));

        let sequence = dialed.borrow();
        assert_eq!(
            sequence.as_slice(),
            &[owner.to_owned(), survivor.to_owned()],
            "on owner drop the worker must migrate to (re-register in) the survivor"
        );
        Ok(())
    }

    /// A connect failure on the current candidate advances + backs off, then a
    /// later candidate succeeds and serves to a stop — proving the loop tolerates
    /// a survivor whose listener is not up at the first attempt.
    #[test]
    fn retryable_connect_failure_backs_off_then_succeeds() -> Result<(), RedialError> {
        let mut ring = cursor(&["down", "up"])?;
        let mut backoff = RedialBackoff::new(Duration::from_millis(1), Duration::from_millis(4));
        let sleeps: RefCell<Vec<Duration>> = RefCell::new(Vec::new());
        let attempts = RefCell::new(0u32);

        let connect = |address: &str| -> Result<String, &'static str> {
            *attempts.borrow_mut() += 1;
            if address == "down" {
                Err("listener not up yet")
            } else {
                Ok(address.to_owned())
            }
        };
        let serve = |_connection: String| ServeResult::Stopped;

        let result = run_redial_loop(
            &mut ring,
            &mut backoff,
            connect,
            serve,
            |duration| sleeps.borrow_mut().push(duration),
            || false,
            |_err| true,
        );
        assert_eq!(result, Ok(()));

        assert!(
            *attempts.borrow() >= 2,
            "the down candidate failed before the up one served"
        );
        assert_eq!(
            sleeps.borrow().first().copied(),
            Some(Duration::from_millis(1)),
            "the first connect failure backs off by the initial pause"
        );
        Ok(())
    }

    /// A non-retryable connect failure (e.g. a deterministic registration
    /// rejection) is surfaced immediately rather than looped forever.
    #[test]
    fn non_retryable_connect_failure_is_surfaced() -> Result<(), RedialError> {
        let mut ring = cursor(&["only"])?;
        let mut backoff = RedialBackoff::new(Duration::from_millis(1), Duration::from_millis(4));

        let connect = |_address: &str| -> Result<String, &'static str> { Err("rejected") };
        let serve = |_connection: String| ServeResult::Stopped;

        let result = run_redial_loop(
            &mut ring,
            &mut backoff,
            connect,
            serve,
            |_| {},
            || false,
            |_err| false,
        );

        assert_eq!(result, Err("rejected"));
        Ok(())
    }

    /// The stop condition is honoured before any dial, so a worker asked to stop
    /// never opens a connection.
    #[test]
    fn stop_before_first_dial_serves_nothing() -> Result<(), RedialError> {
        let mut ring = cursor(&["a"])?;
        let mut backoff = RedialBackoff::new(Duration::from_millis(1), Duration::from_millis(4));
        let dialed = RefCell::new(false);

        let connect = |_address: &str| -> Result<(), &'static str> {
            *dialed.borrow_mut() = true;
            Ok(())
        };
        let serve = |()| ServeResult::Stopped;

        let result = run_redial_loop(
            &mut ring,
            &mut backoff,
            connect,
            serve,
            |_| {},
            || true,
            |_err| true,
        );

        assert_eq!(result, Ok(()));
        assert!(
            !*dialed.borrow(),
            "a stop before the first dial opens no connection"
        );
        Ok(())
    }
}
