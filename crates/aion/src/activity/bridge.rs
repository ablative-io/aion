//! Global activity dispatch bridge for the `aion_flow_ffi:run_activity` NIF.
//!
//! The bridge decouples the raw NIF function pointer (which cannot capture
//! state) from the engine's activity execution path. A concrete dispatcher
//! is installed once during engine build; the NIF accesses it through a
//! process-wide `OnceLock`.

use std::sync::{Arc, OnceLock};

/// Executes an activity request originating from workflow code.
///
/// Implementations receive the activity name, JSON-encoded input, and
/// JSON-encoded config as strings — the same wire format that the Gleam SDK
/// sends through the `aion_flow_ffi:run_activity/3` binding. The return
/// value is the JSON-encoded activity result or a prefixed error string
/// matching the SDK's error-decoding convention.
pub trait ActivityDispatcher: Send + Sync {
    /// Dispatch the named activity and block until completion.
    ///
    /// Returns `Ok(encoded_output)` on success or `Err(error_string)` on
    /// failure. Both sides are strings matching the Gleam SDK's
    /// `Result(String, String)` FFI contract.
    ///
    /// # Errors
    ///
    /// Returns the error string surfaced by the activity execution path —
    /// worker rejection, decode failure, timeout, or activity body error.
    fn dispatch(&self, name: &str, input: &str, config: &str) -> Result<String, String>;
}

static DISPATCHER: OnceLock<Arc<dyn ActivityDispatcher>> = OnceLock::new();

/// Install the activity dispatcher for the engine's NIF bridge.
///
/// Must be called exactly once before any workflow process runs. Subsequent
/// calls are silently ignored — the first installed dispatcher wins.
pub fn install_activity_dispatcher(dispatcher: Arc<dyn ActivityDispatcher>) {
    let _ = DISPATCHER.set(dispatcher);
}

/// Look up the installed activity dispatcher.
pub(crate) fn activity_dispatcher() -> Option<Arc<dyn ActivityDispatcher>> {
    DISPATCHER.get().cloned()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{ActivityDispatcher, activity_dispatcher, install_activity_dispatcher};

    struct Echo;

    impl ActivityDispatcher for Echo {
        fn dispatch(&self, _name: &str, input: &str, _config: &str) -> Result<String, String> {
            Ok(input.to_owned())
        }
    }

    #[test]
    fn dispatcher_is_accessible_after_install() {
        install_activity_dispatcher(Arc::new(Echo));
        let dispatcher = activity_dispatcher();
        assert!(dispatcher.is_some());
        assert_eq!(
            dispatcher
                .as_ref()
                .and_then(|d| d.dispatch("test", "hello", "{}").ok()),
            Some("hello".to_owned())
        );
    }
}
