//! Deliverable 1 pin: `aion_core::Event`'s serde field names still match the
//! six literals the normalizer keys on. Decision 11 is law — the normalizer
//! is allowed to rewrite exactly `recorded_at`, `fire_at`, `workflow_id`,
//! `run_id`/`parent_run_id`, and `package_version`. If any of those field
//! names drifts on main, the normalizer would silently stop normalizing it
//! and a wall-clock/identity field would masquerade as a real divergence (or
//! worse, a real divergence would hide behind a renamed identity field). This
//! test freezes the wire names against a hand-built event so that drift fails
//! loudly here rather than in the differential.

use std::collections::BTreeSet;

use aion_core::{Event, EventEnvelope, PackageVersion, Payload, RunId, TimerId, WorkflowId};
use chrono::DateTime;
use serde_json::Value;

use crate::trail_norm;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A deterministic envelope carrying the two envelope-level identity fields.
fn envelope(seq: u64) -> EventEnvelope {
    EventEnvelope {
        seq,
        recorded_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap_or_default(),
        workflow_id: WorkflowId::new_v4(),
    }
}

/// Recursively collects every object key that appears anywhere in `value`.
fn keys(value: &Value, into: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, field) in map {
                into.insert(key.clone());
                keys(field, into);
            }
        }
        Value::Array(items) => {
            for item in items {
                keys(item, into);
            }
        }
        _ => {}
    }
}

/// The five normalized field families expand to these six wire field names.
const NORMALIZED_FIELDS: &[&str] = &[
    "recorded_at",
    "fire_at",
    "workflow_id",
    "run_id",
    "parent_run_id",
    "package_version",
];

/// A `WorkflowStarted` (carrying `recorded_at`, `workflow_id`, `run_id`,
/// `parent_run_id`, `package_version`) and a `TimerStarted` (carrying
/// `fire_at`) together exhibit all six wire names the normalizer keys on.
#[test]
fn event_serde_field_names_match_the_normalizer_literals() -> TestResult {
    let started = Event::WorkflowStarted {
        envelope: envelope(0),
        workflow_type: String::from("pin"),
        input: Payload::from_json(&serde_json::json!({ "pin": true }))?,
        run_id: RunId::new_v4(),
        parent_run_id: Some(RunId::new_v4()),
        package_version: PackageVersion::new("a".repeat(64)),
    };
    let timer = Event::TimerStarted {
        envelope: envelope(1),
        timer_id: TimerId::anonymous(1),
        fire_at: DateTime::from_timestamp(1_700_000_100, 0).unwrap_or_default(),
    };

    let mut present = BTreeSet::new();
    keys(&serde_json::to_value(&started)?, &mut present);
    keys(&serde_json::to_value(&timer)?, &mut present);

    for field in NORMALIZED_FIELDS {
        assert!(
            present.contains(*field),
            "Event no longer serializes the `{field}` field the normalizer keys on \
             (decision 11): present keys = {present:?}"
        );
    }
    Ok(())
}

/// The normalizer actually rewrites those six fields to their placeholders,
/// leaving the concrete run/time/hash values out of the compared trail.
#[test]
fn normalizer_replaces_every_identity_field() -> TestResult {
    let started = Event::WorkflowStarted {
        envelope: envelope(0),
        workflow_type: String::from("pin"),
        input: Payload::from_json(&serde_json::json!({ "pin": true }))?,
        run_id: RunId::new_v4(),
        parent_run_id: Some(RunId::new_v4()),
        package_version: PackageVersion::new("b".repeat(64)),
    };
    let timer = Event::TimerStarted {
        envelope: envelope(1),
        timer_id: TimerId::anonymous(1),
        fire_at: DateTime::from_timestamp(1_700_000_100, 0).unwrap_or_default(),
    };

    let normalized = trail_norm::normalized_trail(&[started, timer])?;
    let flattened = serde_json::to_string(&normalized)?;

    assert!(flattened.contains("<workflow-id>"), "{flattened}");
    assert!(flattened.contains("<package-version>"), "{flattened}");
    assert!(flattened.contains("<time>"), "{flattened}");
    assert!(flattened.contains("<run-0>"), "{flattened}");
    // First-appearance ordering assigns the run id `<run-0>` and the parent
    // run id `<run-1>`.
    assert!(flattened.contains("<run-1>"), "{flattened}");
    Ok(())
}
