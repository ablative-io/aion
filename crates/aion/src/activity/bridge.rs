//! Activity dispatch bridge for the `aion_flow_ffi` activity NIFs.
//!
//! The bridge decouples the raw NIF function pointer (which cannot capture
//! state) from the engine's activity execution path. A concrete dispatcher
//! is installed on the engine's NIF state during build; the NIF recovers it
//! from its calling context's NIF private data.

use std::sync::Arc;

use futures::future::BoxFuture;

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
    /// The default runs the synchronous [`Self::dispatch_from_process`] on
    /// the runtime's blocking pool via [`tokio::task::spawn_blocking`], so a
    /// dispatcher implementation that blocks its calling thread cannot wedge
    /// the engine's async workers (a single-threaded engine runtime keeps
    /// servicing queries and completions while the dispatch waits).
    /// Nonblocking dispatchers can override this method directly.
    ///
    /// Must be awaited inside a Tokio runtime context; the engine's
    /// completion task guarantees that.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::dispatch_from_process`], plus a
    /// dispatch-failure reason when the blocking task itself is cancelled or
    /// panics.
    fn dispatch_async_from_process(
        self: Arc<Self>,
        name: String,
        input: String,
        config: String,
        caller_pid: Option<u64>,
    ) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(async move {
            let blocking = tokio::task::spawn_blocking(move || {
                self.dispatch_from_process(&name, &input, &config, caller_pid)
            });
            match blocking.await {
                Ok(result) => result,
                Err(join_error) => Err(format!("activity dispatch task failed: {join_error}")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::ActivityDispatcher;
    use crate::runtime::EngineNifState;

    struct Echo;

    impl ActivityDispatcher for Echo {
        fn dispatch(&self, _name: &str, input: &str, _config: &str) -> Result<String, String> {
            Ok(input.to_owned())
        }
    }

    #[test]
    fn dispatcher_is_accessible_after_install_on_engine_state() {
        let state = EngineNifState::default();
        state.set_activity_dispatcher(Arc::new(Echo));
        let dispatcher = state.activity_dispatcher();
        assert!(dispatcher.is_some());
        assert_eq!(
            dispatcher
                .as_ref()
                .and_then(|d| d.dispatch("test", "hello", "{}").ok()),
            Some("hello".to_owned())
        );
    }
}
