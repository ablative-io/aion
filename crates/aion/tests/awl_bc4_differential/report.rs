//! The divergence report (Deliverable 5). The harness accumulates every
//! normalized-trail divergence (fixture, event index, JSON pointer, both
//! values) and every out-of-intersection refusal (fixture, refusal text).
//!
//! Decision 11 is law: a divergence is never normalized away. The
//! differential asserts on an EMPTY divergence set with the full report in
//! the failure message, so any sixth differing field family surfaces loudly
//! for the Fable seat to adjudicate. Refusals are compared against a pinned
//! expected list (ratchet style), so intersection shrinkage is equally loud.

use serde_json::Value;

/// One concrete field-level disagreement between the reference and direct
/// normalized trails for a fixture.
#[derive(Clone, Debug)]
pub struct Divergence {
    /// Fixture path (relative to `tests/fixtures/rev2`, no extension).
    pub fixture: String,
    /// Index of the event whose normalized JSON differs.
    pub event_index: usize,
    /// RFC-6901 JSON pointer into the event where the values diverge.
    pub pointer: String,
    /// The reference (Gleam-built) value at that pointer.
    pub reference: Value,
    /// The direct (`select`-spliced) value at that pointer.
    pub direct: Value,
}

/// One fixture that `lower` refused: out-of-intersection, recorded not failed.
#[derive(Clone, Debug)]
pub struct Refusal {
    /// Fixture path (relative to `tests/fixtures/rev2`, no extension).
    pub fixture: String,
    /// The refusal's `Display` text (`LowerError`).
    pub text: String,
}

/// The accumulated differential outcome across a fixture set.
#[derive(Default)]
pub struct Report {
    /// Fixtures that COMPLETED in both backends with byte-identical trails.
    pub succeeded: Vec<String>,
    /// Fixtures that FAILED terminally (`WorkflowFailed`) in both backends with
    /// byte-identical trails.
    pub failed: Vec<String>,
    /// Fixtures that were CANCELLED terminally (`WorkflowCancelled`) in both
    /// backends. Its own bucket so a `WorkflowFailed` -> `WorkflowCancelled`
    /// regression cannot hide in `failed`.
    pub cancelled: Vec<String>,
    /// Fixtures parked at a durable TIMER boundary in both backends, proven by
    /// visibility `Running` + a specific pending `TimerStarted`. Each entry is
    /// `(fixture, sorted pending-timer identities)` — the timer's identity, not
    /// just "some timer".
    pub parked_timer: Vec<(String, Vec<String>)>,
    /// Fixtures excluded from the oracle: their park boundary is not provable
    /// through any engine surface without racy sampling (out-of-oracle).
    pub excluded: Vec<String>,
    /// Fixtures with at least one divergence (deduplicated, input order).
    pub diverged: Vec<String>,
    /// Every field-level divergence found (must be empty to pass).
    pub divergences: Vec<Divergence>,
    /// Every out-of-intersection refusal, in fixture order.
    pub refusals: Vec<Refusal>,
    /// Infrastructure failures — missing package/bytes, a splice-binding
    /// violation, an engine build/start/read error, a `Stuck` run, a
    /// serialization error, or a disposition the harness cannot classify. This
    /// bucket MUST be empty to pass; nothing here is ever quiesced away.
    pub infra: Vec<(String, String)>,
}

impl Report {
    /// Records a fixture that completed successfully with identical trails.
    pub fn record_succeeded(&mut self, fixture: &str) {
        self.succeeded.push(fixture.to_owned());
    }

    /// Records a fixture that failed terminally with identical trails.
    pub fn record_failed(&mut self, fixture: &str) {
        self.failed.push(fixture.to_owned());
    }

    /// Records a fixture that was cancelled terminally with identical trails.
    pub fn record_cancelled(&mut self, fixture: &str) {
        self.cancelled.push(fixture.to_owned());
    }

    /// Records a fixture parked at a durable timer, with its pending-timer
    /// identities as positive evidence.
    pub fn record_parked_timer(&mut self, fixture: &str, mut pending: Vec<String>) {
        pending.sort();
        self.parked_timer.push((fixture.to_owned(), pending));
    }

    /// Records a fixture excluded from the oracle (out-of-oracle).
    pub fn record_excluded(&mut self, fixture: &str) {
        self.excluded.push(fixture.to_owned());
    }

    /// Records an infrastructure failure (hard — the differential must fail).
    pub fn record_infra(&mut self, fixture: &str, reason: &str) {
        self.infra.push((fixture.to_owned(), reason.to_owned()));
    }

    /// Records an out-of-intersection refusal.
    pub fn record_refusal(&mut self, fixture: &str, text: String) {
        self.refusals.push(Refusal {
            fixture: fixture.to_owned(),
            text,
        });
    }

    /// Fixtures whose full or partial trails compared byte-identical (completed,
    /// failed, cancelled, or timer-parked) — the successful-comparison set.
    pub fn identical_count(&self) -> usize {
        self.succeeded.len() + self.failed.len() + self.cancelled.len() + self.parked_timer.len()
    }

    /// Diffs two normalized trails and records every divergence found, marking
    /// the fixture diverged. Returns the number of divergences appended, so the
    /// caller can classify a clean comparison as identical or unsettled.
    pub fn compare(&mut self, fixture: &str, reference: &[Value], direct: &[Value]) -> usize {
        let before = self.divergences.len();
        if reference.len() != direct.len() {
            self.divergences.push(Divergence {
                fixture: fixture.to_owned(),
                event_index: reference.len().min(direct.len()),
                pointer: String::from("/(trail length)"),
                reference: Value::from(reference.len()),
                direct: Value::from(direct.len()),
            });
        }
        for (index, (left, right)) in reference.iter().zip(direct.iter()).enumerate() {
            let mut found = Vec::new();
            diff_values(left, right, String::new(), &mut found);
            for (pointer, reference_value, direct_value) in found {
                self.divergences.push(Divergence {
                    fixture: fixture.to_owned(),
                    event_index: index,
                    pointer,
                    reference: reference_value,
                    direct: direct_value,
                });
            }
        }
        let appended = self.divergences.len() - before;
        if appended > 0 {
            self.diverged.push(fixture.to_owned());
        }
        appended
    }

    /// A one-line-per-item human rendering for a test failure message.
    pub fn render(&self) -> String {
        let mut lines = vec![format!(
            "differential report: {} succeeded, {} failed-path, {} cancelled, \
             {} timer-parked, {} excluded, {} refused, {} DIVERGENCES, {} INFRA",
            self.succeeded.len(),
            self.failed.len(),
            self.cancelled.len(),
            self.parked_timer.len(),
            self.excluded.len(),
            self.refusals.len(),
            self.divergences.len(),
            self.infra.len(),
        )];
        for (fixture, reason) in &self.infra {
            lines.push(format!("  INFRA {fixture} :: {reason}"));
        }
        for divergence in &self.divergences {
            lines.push(format!(
                "  DIVERGENCE {} event[{}] {}: reference={} direct={}",
                divergence.fixture,
                divergence.event_index,
                divergence.pointer,
                divergence.reference,
                divergence.direct,
            ));
        }
        for refusal in &self.refusals {
            lines.push(format!("  refused {} :: {}", refusal.fixture, refusal.text));
        }
        for (fixture, timers) in &self.parked_timer {
            lines.push(format!("  timer-parked {fixture} on {timers:?}"));
        }
        for excluded in &self.excluded {
            lines.push(format!("  excluded {excluded}"));
        }
        lines.push(String::new());
        lines.join("\n")
    }

    /// The sorted `(fixture, pending-timer identities)` pairs for timer parks.
    pub fn timer_parked_evidence(&self) -> Vec<(String, Vec<String>)> {
        let mut parked = self.parked_timer.clone();
        parked.sort();
        parked
    }

    /// The sorted set of timer-parked fixture names.
    pub fn timer_parked_fixtures(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .parked_timer
            .iter()
            .map(|(fixture, _)| fixture.clone())
            .collect();
        names.sort();
        names
    }

    /// The sorted set of excluded (out-of-oracle) fixture names.
    pub fn excluded_fixtures(&self) -> Vec<String> {
        let mut excluded = self.excluded.clone();
        excluded.sort();
        excluded
    }

    /// The sorted set of fixtures that failed terminally, for the pinned
    /// error-path assertion.
    pub fn failed_fixtures(&self) -> Vec<String> {
        let mut failed = self.failed.clone();
        failed.sort();
        failed
    }

    /// The sorted set of fixtures that completed successfully.
    pub fn succeeded_fixtures(&self) -> Vec<String> {
        let mut succeeded = self.succeeded.clone();
        succeeded.sort();
        succeeded
    }

    /// The sorted set of fixtures that were cancelled.
    pub fn cancelled_fixtures(&self) -> Vec<String> {
        let mut cancelled = self.cancelled.clone();
        cancelled.sort();
        cancelled
    }

    /// The sorted set of refused fixture paths, for the ratchet assertion.
    pub fn refused_fixtures(&self) -> Vec<String> {
        let mut refused: Vec<String> = self
            .refusals
            .iter()
            .map(|refusal| refusal.fixture.clone())
            .collect();
        refused.sort();
        refused
    }
}

/// Recursively records every leaf disagreement between two JSON values as
/// `(json-pointer, left, right)` triples. Objects diff key-wise (a key present
/// on one side only is a divergence); arrays diff position-wise; scalars diff
/// by equality.
fn diff_values(
    left: &Value,
    right: &Value,
    pointer: String,
    found: &mut Vec<(String, Value, Value)>,
) {
    match (left, right) {
        (Value::Object(left_map), Value::Object(right_map)) => {
            let mut keys: Vec<&String> = left_map.keys().chain(right_map.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let child = format!("{pointer}/{}", escape_pointer(key));
                match (left_map.get(key), right_map.get(key)) {
                    (Some(left_child), Some(right_child)) => {
                        diff_values(left_child, right_child, child, found);
                    }
                    (left_child, right_child) => found.push((
                        child,
                        left_child.cloned().unwrap_or(Value::Null),
                        right_child.cloned().unwrap_or(Value::Null),
                    )),
                }
            }
        }
        (Value::Array(left_items), Value::Array(right_items)) => {
            if left_items.len() != right_items.len() {
                found.push((
                    format!("{pointer}/(length)"),
                    Value::from(left_items.len()),
                    Value::from(right_items.len()),
                ));
            }
            for (index, (left_item, right_item)) in
                left_items.iter().zip(right_items.iter()).enumerate()
            {
                diff_values(left_item, right_item, format!("{pointer}/{index}"), found);
            }
        }
        _ => {
            if left != right {
                found.push((pointer, left.clone(), right.clone()));
            }
        }
    }
}

/// Escapes a key for an RFC-6901 JSON pointer segment.
fn escape_pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}
