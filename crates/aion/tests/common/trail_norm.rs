//! Durable-trail normalizer — the seed of the BC-4 differential harness.
//!
//! Two runs of the SAME workflow (possibly executed from two different
//! bytecode productions of it) legitimately differ only in run identity and
//! wall-clock fields: the workflow id, the run ids, every recorded timestamp
//! (`recorded_at` and the derived timer `fire_at`), and the package content
//! hash the run was resolved against (`package_version` — two byte-different
//! productions of the same module necessarily hash differently). Everything
//! else in the durable event trail must be identical, or the two productions
//! are not behaviourally equivalent.
//!
//! This module serializes each [`Event`] to canonical JSON and replaces
//! exactly those identity fields with stable placeholders. Run ids are mapped
//! to placeholders in first-appearance order, so trails that involve several
//! runs (continue-as-new, children) still compare positionally.

use std::collections::HashMap;

use aion_core::Event;
use serde_json::{Value, json};

/// Normalizes a durable event trail for cross-production comparison.
///
/// # Errors
///
/// Returns the underlying `serde_json` error when an event fails to
/// serialize (events are plain data; this indicates a bug, not bad input).
pub fn normalized_trail(events: &[Event]) -> Result<Vec<Value>, serde_json::Error> {
    let mut run_ids: HashMap<String, String> = HashMap::new();
    events
        .iter()
        .map(|event| {
            let mut value = serde_json::to_value(event)?;
            normalize(&mut value, &mut run_ids);
            Ok(value)
        })
        .collect()
}

/// Recursively replaces identity fields with placeholders, leaving every
/// other field untouched.
fn normalize(value: &mut Value, run_ids: &mut HashMap<String, String>) {
    match value {
        Value::Object(map) => {
            for (key, field) in map.iter_mut() {
                match key.as_str() {
                    "recorded_at" | "fire_at" => *field = json!("<time>"),
                    "workflow_id" => *field = json!("<workflow-id>"),
                    "package_version" => *field = json!("<package-version>"),
                    "run_id" | "parent_run_id" => normalize_run_id(field, run_ids),
                    _ => normalize(field, run_ids),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize(item, run_ids);
            }
        }
        _ => {}
    }
}

/// Maps a concrete run id to a `<run-N>` placeholder assigned in
/// first-appearance order. A `null` (absent parent run) is left as `null`.
fn normalize_run_id(field: &mut Value, run_ids: &mut HashMap<String, String>) {
    let Value::String(id) = field else {
        return;
    };
    let next = format!("<run-{}>", run_ids.len());
    let placeholder = run_ids.entry(id.clone()).or_insert(next).clone();
    *field = Value::String(placeholder);
}
