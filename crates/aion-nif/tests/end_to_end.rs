//! End-to-end declaration tests for illustrative `aion-nif` sets.

use std::sync::Arc;

use aion_core::{ActivityError, ActivityErrorKind};
use aion_nif::{
    Determinism, FromTerm, IntoTerm, NifDeclError, NifSet, TermError, activity_nif,
    deterministic_nif, from_term_via_payload,
};
use beamr::{atom::AtomTable, native::ProcessContext};

fn context() -> ProcessContext<'static> {
    let mut ctx = ProcessContext::new();
    ctx.set_atom_table(Some(Arc::new(AtomTable::with_common_atoms())));
    ctx
}

fn retryable_activity_failure() -> Result<String, ActivityError> {
    Err(ActivityError {
        kind: ActivityErrorKind::Retryable,
        message: "illustrative retryable failure".to_owned(),
        details: None,
    })
}

#[test]
fn illustrative_nif_set_exposes_descriptors_and_dispatches() -> Result<(), TermError> {
    let pure = deterministic_nif!("example/module", "shout", |input: String| -> String {
        input.to_uppercase()
    });
    let activity = activity_nif!("example/module", "fetch_label", |name: String| -> Result<
        String,
        ActivityError,
    > {
        Ok(format!("activity:{name}"))
    });
    let activity_failure = activity_nif!(
        "example/module", "fetch_label_failure", retryable_activity_failure, () -> Result<String, ActivityError>
    );

    let set = NifSet::builder("example/module")
        .with_nif(pure)
        .with_nif(activity)
        .with_nif(activity_failure)
        .build()
        .map_err(|e| decl_error_to_term_error(&e))?;

    assert_eq!(set.module(), "example/module");
    assert_eq!(set.nifs().len(), 3);

    let pure = &set.nifs()[0];
    assert_eq!(pure.module(), "example/module");
    assert_eq!(pure.function(), "shout");
    assert_eq!(pure.arity(), 1);
    assert_eq!(pure.determinism(), Determinism::Pure);
    assert!(!pure.is_dirty());

    let activity = &set.nifs()[1];
    assert_eq!(activity.module(), "example/module");
    assert_eq!(activity.function(), "fetch_label");
    assert_eq!(activity.arity(), 1);
    assert_eq!(activity.determinism(), Determinism::SideEffectful);
    assert!(activity.is_dirty());

    let activity_failure = &set.nifs()[2];
    assert_eq!(activity_failure.function(), "fetch_label_failure");
    assert_eq!(activity_failure.arity(), 0);
    assert_eq!(activity_failure.determinism(), Determinism::SideEffectful);
    assert!(activity_failure.is_dirty());

    let mut ctx = context();
    let mut arg_ctx = context();
    let mut nif_ctx = aion_nif::NifContext::new(&mut arg_ctx);
    let pure_input = "hello nif".to_owned().into_term(&mut nif_ctx)?;
    let pure_output =
        (pure.native())(&[pure_input], &mut ctx).map_err(native_error_to_term_error)?;
    assert_eq!(String::from_term(pure_output, &ctx)?, "HELLO NIF");

    let activity_input = "value".to_owned().into_term(&mut nif_ctx)?;
    let activity_output =
        (activity.native())(&[activity_input], &mut ctx).map_err(native_error_to_term_error)?;
    assert_eq!(String::from_term(activity_output, &ctx)?, "activity:value");

    let failure_term =
        (activity_failure.native())(&[], &mut ctx)
            .err()
            .ok_or(TermError::HeapAllocation {
                shape: "activity error",
            })?;
    let decoded_failure: ActivityError = from_term_via_payload(failure_term, &ctx)?;
    assert_eq!(decoded_failure.kind, ActivityErrorKind::Retryable);
    assert!(decoded_failure.is_retryable());
    assert_eq!(decoded_failure.message, "illustrative retryable failure");

    Ok(())
}

fn decl_error_to_term_error(error: &NifDeclError) -> TermError {
    TermError::Conversion {
        context: "illustrative NifSet declaration",
        message: error.to_string(),
    }
}

fn native_error_to_term_error(term: beamr::term::Term) -> TermError {
    TermError::Conversion {
        context: "illustrative native invocation",
        message: format!("unexpected native error term: {term:?}"),
    }
}
