//! The one deterministic activity dispatcher shared by BOTH backends of every
//! differential run. It is a pure function of `(activity name, activity
//! input)`, so any difference between the reference and direct trails can only
//! come from backend behavior, never from the dispatcher.
//!
//! The canned result is a stable echo of the activity type plus the parsed
//! input (`{"awl_bc4": <name>, "input": <input>}`). Whether the generated
//! output codec accepts that echo or rejects it, it does so IDENTICALLY on
//! both backends (the codecs are what BC-2/BC-3 proved equivalent), so the
//! resulting trail — a completion or a codec-driven failure — is byte-identical
//! either way. That is exactly the property the differential asserts.

use aion::activity::bridge::{ActivityDispatch, ActivityDispatcher};
use serde_json::{Value, json};

/// A deterministic, stateless echo dispatcher.
pub struct EchoDispatcher;

impl ActivityDispatcher for EchoDispatcher {
    fn dispatch(&self, request: ActivityDispatch) -> Result<String, String> {
        // Parse the input so the echo carries structured data; fall back to
        // the raw string when it is not JSON. Both paths are deterministic.
        let input = serde_json::from_str::<Value>(request.input.as_str())
            .unwrap_or_else(|_| Value::String(request.input.clone()));
        Ok(json!({
            "awl_bc4": request.name,
            "input": input,
        })
        .to_string())
    }
}
