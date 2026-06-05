//! NIF declaration builders for deterministic and side-effectful paths.

use std::panic::{AssertUnwindSafe, catch_unwind};

use beamr::{
    native::{NativeFn, ProcessContext},
    term::Term,
};

use crate::{Determinism, FromTerm, IntoTerm, Nif, TermError, raw};

/// Builds a deterministic NIF descriptor around a generated typed shim.
///
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
    Nif::new(module, function, arity, native, false, Determinism::Pure)
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

/// Declares a pure deterministic NIF from a typed Rust body.
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
            let $arg = match $crate::declare::decode_argument::<$arg_ty>(args[0], ctx, 0) {
                Ok(value) => value,
                Err(error) => {
                    $crate::declare::begin_nif_call();
                    return Err($crate::declare::term_error_to_term(&error, ctx));
                }
            };
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
            let $left = match $crate::declare::decode_argument::<$left_ty>(args[0], ctx, 0) {
                Ok(value) => value,
                Err(error) => {
                    $crate::declare::begin_nif_call();
                    return Err($crate::declare::term_error_to_term(&error, ctx));
                }
            };
            let $right = match $crate::declare::decode_argument::<$right_ty>(args[1], ctx, 1) {
                Ok(value) => value,
                Err(error) => {
                    $crate::declare::begin_nif_call();
                    return Err($crate::declare::term_error_to_term(&error, ctx));
                }
            };
            $crate::declare::begin_nif_call();
            $crate::declare::invoke_pure(ctx, || -> $ret { $($body)* })
        }

        $crate::declare::pure_descriptor($module, $function, 2, __aion_nif_shim)
    }};
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use beamr::{atom::AtomTable, native::ProcessContext, term::Term};

    use crate::{Determinism, FromTerm, IntoTerm, TermError};

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

        let error = (nif.native())(&[only], &mut ctx).err().ok_or(TermError::HeapAllocation { shape: "test" })?;
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
            .err().ok_or(TermError::HeapAllocation { shape: "test" })?;
        let message = decode_error_term(error, &ctx)?;

        assert!(message.contains("failed to decode argument 1"));
        assert!(message.contains("utf8 binary") || message.contains("binary"));
        Ok(())
    }

    #[test]
    fn deterministic_nif_contains_author_panic() -> Result<(), TermError> {
        let nif = deterministic_nif!("example/module", "explode", || -> String { panic!("boom") });
        let mut ctx = context();

        let error = (nif.native())(&[], &mut ctx).err().ok_or(TermError::HeapAllocation { shape: "test" })?;
        let message = decode_error_term(error, &ctx)?;

        assert!(message.contains("panicked"));
        assert!(message.contains("boom"));
        Ok(())
    }
}
