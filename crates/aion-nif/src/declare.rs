//! NIF declaration builders for deterministic and side-effectful paths.

use std::panic::{AssertUnwindSafe, catch_unwind};

use aion_core::{ActivityError, ActivityErrorKind};
use beamr::{
    native::{NativeFn, ProcessContext},
    scheduler::Scheduler,
    term::Term,
};

use crate::{FromTerm, IntoTerm, Nif, TermError, into_term_via_payload, raw};

/// Builds a deterministic NIF descriptor around a generated typed shim.
///
/// Determinism is fixed by this builder: it can only emit [`Determinism::Pure`].
/// The [`deterministic_nif!`] macro is the public declaration surface. Its shim
/// checks arity, decodes each positional argument with [`FromTerm`], invokes the
/// typed author body, and encodes the return value with [`IntoTerm`]. It also
/// catches panics at the native boundary so a single buggy NIF cannot unwind
/// across beamr's scheduler/native FFI boundary.
#[must_use]
pub fn pure_descriptor(
    module: impl Into<String>,
    function: impl Into<String>,
    arity: u8,
    native: NativeFn,
) -> Nif {
    Nif::pure(module, function, arity, native)
}

/// Builds a side-effectful activity NIF descriptor around a generated typed shim.
///
/// Determinism is fixed by this builder: it can only emit
/// [`Determinism::SideEffectful`]. Side-effectful NIFs are activity bodies, not
/// inline helpers. The engine invokes them through the activity contract so a
/// completed or failed result is recorded once and returned from history on
/// replay, never re-run as inline workflow code.
///
/// The descriptor is dirty by default because side effects may block, and a
/// blocking native function must not occupy a normal BEAM scheduler.
#[must_use]
pub fn activity_descriptor(
    module: impl Into<String>,
    function: impl Into<String>,
    arity: u8,
    native: NativeFn,
) -> Nif {
    Nif::side_effectful(module, function, arity, native)
}

/// Releases retained heap storage from the previous generated NIF invocation.
///
/// The raw seam retains heap-backed return terms long enough for the caller to
/// consume them. Until a scoped per-invocation owner exists, generated shims keep
/// incoming terms alive through arity and argument decoding, then clear old
/// retained storage immediately before encoding this invocation's result or error.
#[doc(hidden)]
pub fn begin_nif_call() {
    raw::clear_term_storage();
}

/// Decodes one typed argument and annotates conversion failures with its index.
///
/// # Errors
///
/// Returns [`TermError::ArgumentDecode`] when `T` cannot be decoded from `term`.
#[doc(hidden)]
pub fn decode_argument<T>(term: Term, ctx: &ProcessContext, index: usize) -> Result<T, TermError>
where
    T: FromTerm,
{
    T::from_term(term, ctx).map_err(|source| TermError::ArgumentDecode {
        index,
        source: Box::new(source),
    })
}

/// Encodes an arity mismatch as a typed NIF error term.
#[doc(hidden)]
pub fn arity_error_term(expected: usize, actual: usize, ctx: &mut ProcessContext) -> Term {
    let error = TermError::Conversion {
        context: "nif arity",
        message: format!("expected {expected} arguments, received {actual}"),
    };
    term_error_to_term(&error, ctx)
}

/// Encodes a conversion error as a typed NIF error term.
#[doc(hidden)]
pub fn term_error_to_term(error: &TermError, ctx: &mut ProcessContext) -> Term {
    error_message_to_term(error.to_string(), ctx)
}

/// Invokes a deterministic author body and converts success, conversion failure,
/// or panic into the beamr `NativeFn` result shape.
///
/// # Errors
///
/// Returns an error term when the body panics or the return value cannot be
/// encoded through [`IntoTerm`].
#[doc(hidden)]
pub fn invoke_pure<R, F>(ctx: &mut ProcessContext, body: F) -> Result<Term, Term>
where
    R: IntoTerm,
    F: FnOnce() -> R,
{
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value
            .into_term(ctx)
            .map_err(|error| term_error_to_term(&error, ctx)),
        Err(payload) => Err(error_message_to_term(
            format!(
                "deterministic NIF body panicked: {}",
                panic_message(payload)
            ),
            ctx,
        )),
    }
}

/// Invokes a side-effectful activity body and converts its typed outcome into
/// the beamr `NativeFn` result shape.
///
/// # Errors
///
/// Returns a structured [`ActivityError`] term when the body reports an activity
/// failure, panics, or the success/error value cannot be encoded. Panic failures
/// are classified as [`ActivityErrorKind::Terminal`] so retry policy evaluation
/// can preserve the invariant that unwinding never crosses the FFI boundary.
#[doc(hidden)]
pub fn invoke_activity<R, F>(ctx: &mut ProcessContext, body: F) -> Result<Term, Term>
where
    R: IntoTerm,
    F: FnOnce() -> Result<R, ActivityError>,
{
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(value)) => value
            .into_term(ctx)
            .map_err(|error| activity_error_to_term(term_error_activity(error), ctx)),
        Ok(Err(error)) => Err(activity_error_to_term(error, ctx)),
        Err(payload) => Err(activity_error_to_term(
            ActivityError {
                kind: ActivityErrorKind::Terminal,
                message: format!("activity NIF body panicked: {}", panic_message(payload)),
                details: None,
            },
            ctx,
        )),
    }
}

/// Encodes an [`ActivityError`] as a JSON-shaped term that preserves kind,
/// message, and details for the engine's activity-failure reconstruction.
#[doc(hidden)]
pub fn activity_error_to_term(error: ActivityError, ctx: &mut ProcessContext) -> Term {
    match into_term_via_payload(error, ctx) {
        Ok(term) => term,
        Err(error) => fallback_activity_error_term(error.to_string(), ctx),
    }
}

fn fallback_activity_error_term(message: String, ctx: &mut ProcessContext) -> Term {
    let error = ActivityError {
        kind: ActivityErrorKind::Terminal,
        message,
        details: None,
    };
    match into_term_via_payload(error, ctx) {
        Ok(term) => term,
        Err(_) => ctx.allocate_term(Term::NIL),
    }
}

fn term_error_activity(error: TermError) -> ActivityError {
    ActivityError {
        kind: ActivityErrorKind::Terminal,
        message: error.to_string(),
        details: None,
    }
}

/// Handle used by a suspending activity NIF to wake the same BEAM process later.
///
/// This is a thin wrapper over beamr's existing `Scheduler::wake_with_result`;
/// it deliberately does not introduce a futures runtime, executor, or any other
/// async model.
pub struct ActivityWakeHandle<'scheduler> {
    scheduler: &'scheduler Scheduler,
    pid: u64,
}

impl ActivityWakeHandle<'_> {
    /// Wake the suspended process with `result` in the process' return register.
    pub fn wake_with_result(&self, result: Term) {
        self.scheduler.wake_with_result(self.pid, result);
    }

    /// Process id captured when the NIF requested suspension.
    #[must_use]
    pub const fn pid(&self) -> u64 {
        self.pid
    }
}

/// Requests beamr suspension for a side-effectful NIF that must wait instead of
/// blocking a dirty scheduler thread.
///
/// This mirrors the beamr-meridian async step pattern: capture the scheduler and
/// calling pid, call [`ProcessContext::request_suspend`], return from the NIF,
/// and later deliver the encoded result through [`Scheduler::wake_with_result`]
/// via the returned [`ActivityWakeHandle`]. This is the only async mechanism in
/// `aion-nif`; the crate does not define a futures/executor model.
///
/// # Errors
///
/// Returns [`TermError::Conversion`] when the process context does not carry a
/// pid, because beamr cannot wake a process without a target pid.
pub fn request_activity_suspend<'scheduler>(
    ctx: &mut ProcessContext,
    scheduler: &'scheduler Scheduler,
    timeout_ms: Option<u64>,
) -> Result<ActivityWakeHandle<'scheduler>, TermError> {
    let pid = ctx.pid().ok_or_else(|| TermError::Conversion {
        context: "activity suspend",
        message: "process context does not contain a pid".to_owned(),
    })?;
    ctx.request_suspend(timeout_ms);
    Ok(ActivityWakeHandle { scheduler, pid })
}

fn error_message_to_term(message: String, ctx: &mut ProcessContext) -> Term {
    match Result::<String, String>::Err(message).into_term(ctx) {
        Ok(term) => term,
        Err(_) => ctx.allocate_term(Term::NIL),
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => (*message).to_owned(),
            Err(_) => "unknown panic payload".to_owned(),
        },
    }
}

#[doc(hidden)]
#[macro_export]
macro_rules! __aion_nif_decode_argument {
    ($args:expr, $ctx:expr, $index:expr, $ty:ty) => {
        match $crate::declare::decode_argument::<$ty>($args[$index], $ctx, $index) {
            Ok(value) => value,
            Err(error) => {
                $crate::declare::begin_nif_call();
                return Err($crate::declare::term_error_to_term(&error, $ctx));
            }
        }
    };
}

/// Declares a pure deterministic NIF from a typed Rust body.
///
/// Determinism is a type-level declaration choice: this macro can only produce
/// [`Determinism::Pure`](crate::Determinism). Pure helpers may be bound inline
/// and re-executed during replay. Side-effectful work must use [`activity_nif!`],
/// which the engine invokes through the recorded activity contract and returns
/// from history on replay instead of re-running.
///
/// The generated shim never exposes raw `&[Term]` to author code. It checks
/// arity, decodes each argument by position, invokes the typed body, encodes the
/// typed return value through [`IntoTerm`], and catches panics before they can
/// unwind across the native boundary.
#[macro_export]
macro_rules! deterministic_nif {
    ($module:expr, $function:expr, $body:expr, () -> $ret:ty) => {
        $crate::deterministic_nif!($module, $function, || -> $ret { $body() })
    };
    ($module:expr, $function:expr, $body:expr, ($arg:ident : $arg_ty:ty) -> $ret:ty) => {
        $crate::deterministic_nif!($module, $function, |$arg: $arg_ty| -> $ret { $body($arg) })
    };
    (
        $module:expr,
        $function:expr,
        $body:expr,
        ($left:ident : $left_ty:ty, $right:ident : $right_ty:ty) -> $ret:ty
    ) => {
        $crate::deterministic_nif!($module, $function, |$left: $left_ty, $right: $right_ty| -> $ret {
            $body($left, $right)
        })
    };
    ($module:expr, $function:expr, || -> $ret:ty { $($body:tt)* }) => {{
        fn __aion_nif_shim(
            args: &[beamr::term::Term],
            ctx: &mut beamr::native::ProcessContext,
        ) -> Result<beamr::term::Term, beamr::term::Term> {
            if args.len() != 0 {
                $crate::declare::begin_nif_call();
                return Err($crate::declare::arity_error_term(0, args.len(), ctx));
            }
            $crate::declare::begin_nif_call();
            $crate::declare::invoke_pure(ctx, || -> $ret { $($body)* })
        }

        $crate::declare::pure_descriptor($module, $function, 0, __aion_nif_shim)
    }};
    ($module:expr, $function:expr, |$arg:ident : $arg_ty:ty| -> $ret:ty { $($body:tt)* }) => {{
        fn __aion_nif_shim(
            args: &[beamr::term::Term],
            ctx: &mut beamr::native::ProcessContext,
        ) -> Result<beamr::term::Term, beamr::term::Term> {
            if args.len() != 1 {
                $crate::declare::begin_nif_call();
                return Err($crate::declare::arity_error_term(1, args.len(), ctx));
            }
            let $arg = $crate::__aion_nif_decode_argument!(args, ctx, 0, $arg_ty);
            $crate::declare::begin_nif_call();
            $crate::declare::invoke_pure(ctx, || -> $ret { $($body)* })
        }

        $crate::declare::pure_descriptor($module, $function, 1, __aion_nif_shim)
    }};
    (
        $module:expr,
        $function:expr,
        |$left:ident : $left_ty:ty, $right:ident : $right_ty:ty| -> $ret:ty { $($body:tt)* }
    ) => {{
        fn __aion_nif_shim(
            args: &[beamr::term::Term],
            ctx: &mut beamr::native::ProcessContext,
        ) -> Result<beamr::term::Term, beamr::term::Term> {
            if args.len() != 2 {
                $crate::declare::begin_nif_call();
                return Err($crate::declare::arity_error_term(2, args.len(), ctx));
            }
            let $left = $crate::__aion_nif_decode_argument!(args, ctx, 0, $left_ty);
            let $right = $crate::__aion_nif_decode_argument!(args, ctx, 1, $right_ty);
            $crate::declare::begin_nif_call();
            $crate::declare::invoke_pure(ctx, || -> $ret { $($body)* })
        }

        $crate::declare::pure_descriptor($module, $function, 2, __aion_nif_shim)
    }};
}

/// Declares a side-effectful native activity NIF from a typed Rust body.
///
/// Determinism is a type-level declaration choice: this macro can only produce
/// [`Determinism::SideEffectful`](crate::Determinism). The body must return
/// `Result<T, aion_core::ActivityError>`, so retryable/terminal failure
/// classification is preserved. The engine must invoke these NIFs through the
/// recorded activity contract; replay returns history instead of re-running.
///
/// The generated shim mirrors [`deterministic_nif!`]: it checks arity, decodes
/// each positional argument with [`FromTerm`], never exposes raw `&[Term]` to
/// author code, encodes `Ok(T)` as the return term, and encodes `Err(ActivityError)`
/// on the native error path.
#[macro_export]
macro_rules! activity_nif {
    ($module:expr, $function:expr, $body:expr, () -> $ret:ty) => {
        $crate::activity_nif!($module, $function, || -> $ret { $body() })
    };
    ($module:expr, $function:expr, $body:expr, ($arg:ident : $arg_ty:ty) -> $ret:ty) => {
        $crate::activity_nif!($module, $function, |$arg: $arg_ty| -> $ret { $body($arg) })
    };
    (
        $module:expr,
        $function:expr,
        $body:expr,
        ($left:ident : $left_ty:ty, $right:ident : $right_ty:ty) -> $ret:ty
    ) => {
        $crate::activity_nif!($module, $function, |$left: $left_ty, $right: $right_ty| -> $ret {
            $body($left, $right)
        })
    };
    ($module:expr, $function:expr, || -> $ret:ty { $($body:tt)* }) => {{
        fn __aion_nif_shim(
            args: &[beamr::term::Term],
            ctx: &mut beamr::native::ProcessContext,
        ) -> Result<beamr::term::Term, beamr::term::Term> {
            if args.len() != 0 {
                $crate::declare::begin_nif_call();
                return Err($crate::declare::arity_error_term(0, args.len(), ctx));
            }
            $crate::declare::begin_nif_call();
            $crate::declare::invoke_activity(ctx, || -> $ret { $($body)* })
        }

        $crate::declare::activity_descriptor($module, $function, 0, __aion_nif_shim)
    }};
    ($module:expr, $function:expr, |$arg:ident : $arg_ty:ty| -> $ret:ty { $($body:tt)* }) => {{
        fn __aion_nif_shim(
            args: &[beamr::term::Term],
            ctx: &mut beamr::native::ProcessContext,
        ) -> Result<beamr::term::Term, beamr::term::Term> {
            if args.len() != 1 {
                $crate::declare::begin_nif_call();
                return Err($crate::declare::arity_error_term(1, args.len(), ctx));
            }
            let $arg = $crate::__aion_nif_decode_argument!(args, ctx, 0, $arg_ty);
            $crate::declare::begin_nif_call();
            $crate::declare::invoke_activity(ctx, || -> $ret { $($body)* })
        }

        $crate::declare::activity_descriptor($module, $function, 1, __aion_nif_shim)
    }};
    (
        $module:expr,
        $function:expr,
        |$left:ident : $left_ty:ty, $right:ident : $right_ty:ty| -> $ret:ty { $($body:tt)* }
    ) => {{
        fn __aion_nif_shim(
            args: &[beamr::term::Term],
            ctx: &mut beamr::native::ProcessContext,
        ) -> Result<beamr::term::Term, beamr::term::Term> {
            if args.len() != 2 {
                $crate::declare::begin_nif_call();
                return Err($crate::declare::arity_error_term(2, args.len(), ctx));
            }
            let $left = $crate::__aion_nif_decode_argument!(args, ctx, 0, $left_ty);
            let $right = $crate::__aion_nif_decode_argument!(args, ctx, 1, $right_ty);
            $crate::declare::begin_nif_call();
            $crate::declare::invoke_activity(ctx, || -> $ret { $($body)* })
        }

        $crate::declare::activity_descriptor($module, $function, 2, __aion_nif_shim)
    }};
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{ActivityError, ActivityErrorKind};
    use beamr::{
        atom::AtomTable,
        module::ModuleRegistry,
        native::{BifRegistryImpl, ProcessContext},
        scheduler::{Scheduler, SchedulerConfig},
        term::Term,
    };

    use crate::{
        Determinism, FromTerm, IntoTerm, TermError, from_term_via_payload, request_activity_suspend,
    };

    fn context() -> ProcessContext {
        let mut ctx = ProcessContext::new();
        ctx.set_atom_table(Some(Arc::new(AtomTable::with_common_atoms())));
        ctx
    }

    fn decode_error_term(term: Term, ctx: &ProcessContext) -> Result<String, TermError> {
        let result = Result::<String, String>::from_term(term, ctx)?;
        match result {
            Ok(message) | Err(message) => Ok(message),
        }
    }

    fn decode_activity_error(term: Term, ctx: &ProcessContext) -> Result<ActivityError, TermError> {
        from_term_via_payload(term, ctx)
    }

    fn scheduler() -> Result<Scheduler, String> {
        Scheduler::with_code_server(
            SchedulerConfig {
                thread_count: Some(1),
            },
            Arc::new(ModuleRegistry::new()),
            Arc::new(AtomTable::with_common_atoms()),
            Arc::new(BifRegistryImpl::new()),
        )
    }

    #[test]
    fn deterministic_nif_declares_pure_helper_and_invokes_typed_body() -> Result<(), TermError> {
        let nif = deterministic_nif!("example/module", "concat", |left: String,
                                                                  right: String|
         -> String {
            format!("{left}{right}")
        });
        let mut ctx = context();
        let left = "hello ".to_owned().into_term(&mut ctx)?;
        let right = "world".to_owned().into_term(&mut ctx)?;

        let output =
            (nif.native())(&[left, right], &mut ctx).map_err(|term| TermError::Conversion {
                context: "test native invocation",
                message: format!("unexpected error term: {term:?}"),
            })?;

        assert_eq!(nif.determinism(), Determinism::Pure);
        assert!(!nif.is_dirty());
        assert_eq!(nif.arity(), 2);
        assert_eq!(String::from_term(output, &ctx)?, "hello world");
        Ok(())
    }

    #[test]
    fn deterministic_nif_reports_wrong_arity() -> Result<(), TermError> {
        let nif = deterministic_nif!("example/module", "concat", |left: String,
                                                                  right: String|
         -> String {
            format!("{left}{right}")
        });
        let mut ctx = context();
        let only = "hello".to_owned().into_term(&mut ctx)?;

        let error = (nif.native())(&[only], &mut ctx)
            .err()
            .ok_or(TermError::HeapAllocation { shape: "test" })?;
        let message = decode_error_term(error, &ctx)?;

        assert!(message.contains("expected 2 arguments"));
        assert!(message.contains("received 1"));
        Ok(())
    }

    #[test]
    fn deterministic_nif_reports_decode_argument_index() -> Result<(), TermError> {
        let nif = deterministic_nif!("example/module", "concat", |left: String,
                                                                  right: String|
         -> String {
            format!("{left}{right}")
        });
        let mut ctx = context();
        let left = "hello".to_owned().into_term(&mut ctx)?;
        let right = 42_i64.into_term(&mut ctx)?;

        let error = (nif.native())(&[left, right], &mut ctx)
            .err()
            .ok_or(TermError::HeapAllocation { shape: "test" })?;
        let message = decode_error_term(error, &ctx)?;

        assert!(message.contains("failed to decode argument 1"));
        assert!(message.contains("utf8 binary") || message.contains("binary"));
        Ok(())
    }

    #[test]
    fn deterministic_nif_contains_author_panic() -> Result<(), TermError> {
        let nif = deterministic_nif!("example/module", "explode", || -> String { panic!("boom") });
        let mut ctx = context();

        let error = (nif.native())(&[], &mut ctx)
            .err()
            .ok_or(TermError::HeapAllocation { shape: "test" })?;
        let message = decode_error_term(error, &ctx)?;

        assert!(message.contains("panicked"));
        assert!(message.contains("boom"));
        Ok(())
    }

    #[test]
    fn activity_nif_declares_dirty_side_effectful_body_and_encodes_ok() -> Result<(), TermError> {
        let nif = activity_nif!("example/module", "read_env", |name: String| -> Result<
            String,
            ActivityError,
        > {
            Ok(format!("env:{name}"))
        });
        let mut ctx = context();
        let name = "AION_TEST_VALUE".to_owned().into_term(&mut ctx)?;

        let output = (nif.native())(&[name], &mut ctx).map_err(|term| TermError::Conversion {
            context: "test activity invocation",
            message: format!("unexpected activity error term: {term:?}"),
        })?;

        assert_eq!(nif.determinism(), Determinism::SideEffectful);
        assert!(nif.is_dirty());
        assert_eq!(nif.arity(), 1);
        assert_eq!(String::from_term(output, &ctx)?, "env:AION_TEST_VALUE");
        Ok(())
    }

    fn retryable_activity_body() -> Result<String, ActivityError> {
        Err(ActivityError {
            kind: ActivityErrorKind::Retryable,
            message: "classified failure".to_owned(),
            details: None,
        })
    }

    #[test]
    fn activity_nif_encodes_retryable_and_terminal_activity_errors() -> Result<(), TermError> {
        let retryable = activity_nif!(
            "example/module", "fail", retryable_activity_body, () -> Result<String, ActivityError>
        );
        let terminal = activity_nif!(
            "example/module",
            "fail",
            || -> Result<String, ActivityError> {
                Err(ActivityError {
                    kind: ActivityErrorKind::Terminal,
                    message: "terminal failure".to_owned(),
                    details: None,
                })
            }
        );

        let mut ctx = context();
        let retryable_error = (retryable.native())(&[], &mut ctx)
            .err()
            .ok_or(TermError::HeapAllocation { shape: "test" })?;
        let decoded_retryable = decode_activity_error(retryable_error, &ctx)?;

        let terminal_error = (terminal.native())(&[], &mut ctx)
            .err()
            .ok_or(TermError::HeapAllocation { shape: "test" })?;
        let decoded_terminal = decode_activity_error(terminal_error, &ctx)?;

        assert_eq!(decoded_retryable.kind, ActivityErrorKind::Retryable);
        assert!(decoded_retryable.is_retryable());
        assert_eq!(decoded_retryable.message, "classified failure");
        assert_eq!(decoded_terminal.kind, ActivityErrorKind::Terminal);
        assert!(!decoded_terminal.is_retryable());
        assert_eq!(decoded_terminal.message, "terminal failure");
        Ok(())
    }

    #[test]
    fn activity_nif_contains_panic_as_terminal_activity_error() -> Result<(), TermError> {
        let nif = activity_nif!(
            "example/module",
            "explode",
            || -> Result<String, ActivityError> { panic!("boom") }
        );
        let mut ctx = context();

        let error = (nif.native())(&[], &mut ctx)
            .err()
            .ok_or(TermError::HeapAllocation { shape: "test" })?;
        let decoded = decode_activity_error(error, &ctx)?;

        assert_eq!(decoded.kind, ActivityErrorKind::Terminal);
        assert!(decoded.message.contains("activity NIF body panicked"));
        assert!(decoded.message.contains("boom"));
        Ok(())
    }

    #[test]
    fn activity_suspend_helper_records_suspend_and_captures_wake_target() -> Result<(), TermError> {
        let scheduler = scheduler().map_err(|message| TermError::Conversion {
            context: "test scheduler",
            message,
        })?;
        let mut ctx = context();
        ctx.set_pid(Some(42));

        let handle = request_activity_suspend(&mut ctx, &scheduler, Some(250))?;
        let suspend = ctx
            .take_suspend()
            .ok_or(TermError::HeapAllocation { shape: "suspend" })?;

        assert_eq!(handle.pid(), 42);
        assert_eq!(suspend.timeout_ms, Some(250));
        handle.wake_with_result("done".to_owned().into_term(&mut ctx)?);
        scheduler.shutdown();
        Ok(())
    }
}
