//! The one deterministic activity dispatcher shared by BOTH backends of every
//! differential run. It is a pure function of the activity name, so any
//! difference between the reference and direct trails can only come from
//! backend behavior, never from the dispatcher.
//!
//! Each canned result is a schema-valid instance of the fixture's declared
//! action return type (`fixtures::action_results`), so the activity's generated
//! output codec ACCEPTS it and the workflow runs its real body — the flagship's
//! `greet -> shout` path actually executes rather than tripping an
//! activity-decode detour. The identical map drives both runs, so a completion
//! or a data-driven failure outcome is byte-identical either way.

use std::collections::HashMap;

use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};

/// A deterministic dispatcher returning the canned schema-valid result for each
/// declared action.
pub struct TypedDispatcher {
    results: HashMap<String, String>,
}

impl TypedDispatcher {
    /// Builds a dispatcher over a fixture's `(action name -> result JSON)` map.
    pub fn new(results: HashMap<String, String>) -> Self {
        Self { results }
    }
}

impl ActivityDispatcher for TypedDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        self.results.get(&request.name).cloned().ok_or_else(|| {
            // Deterministic and identical on both backends: a fixture that
            // dispatches an activity with no declared return type fails the
            // same way on each side (surfaced, never a silent divergence).
            format!("terminal:no canned result for activity `{}`", request.name)
        })
    }
}
