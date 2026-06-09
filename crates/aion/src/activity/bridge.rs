//! Global activity dispatch bridge for the `aion_flow_ffi` activity NIFs.
//!
//! The bridge decouples the raw NIF function pointer (which cannot capture
//! state) from the engine's activity execution path. A concrete dispatcher
//! is installed once during engine build; the NIF accesses it through a
//! process-wide `OnceLock`.

use std::sync::{Arc, OnceLock};

use futures::future::{BoxFuture, ready};

/// Executes an activity request originating from workflow code.
///
/// Implementations receive the activity name, JSON-encoded input, and
/// JSON-encoded config as strings — the same wire format that the Gleam SDK
/// sends through the `aion_flow_ffi:dispatch_activity/3` binding. The return
/// value is the JSON-encoded activity result or a prefixed error string
/// matching the SDK's error-decoding convention.
pub trait ActivityDispatcher: Send + Sync + 'static {
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

    /// Dispatch the named activity with the calling workflow process id when
    /// the runtime can provide it.
    ///
    /// Implementations that need to correlate a raw NIF call back to an active
    /// workflow handle can override this method. The default preserves the
    /// original dispatcher contract for tests and non-workflow callers.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::dispatch`].
    fn dispatch_from_process(
        &self,
        name: &str,
        input: &str,
        config: &str,
        caller_pid: Option<u64>,
    ) -> Result<String, String> {
        let _ = caller_pid;
        self.dispatch(name, input, config)
    }

    /// Dispatch the named activity from a Tokio task.
    ///
    /// The default keeps existing synchronous dispatchers and tests source
    /// compatible while moving the blocking wait off BEAM scheduler threads.
    /// Future nonblocking dispatchers can override this method directly.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::dispatch_from_process`].
    fn dispatch_async_from_process<'a>(
        &'a self,
        name: &'a str,
        input: &'a str,
        config: &'a str,
        caller_pid: Option<u64>,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(ready(
            self.dispatch_from_process(name, input, config, caller_pid),
        ))
    }
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
