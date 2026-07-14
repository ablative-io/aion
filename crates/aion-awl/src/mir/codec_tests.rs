//! In-crate codec REFUSAL pins (BC-2b-5 panel round): compile-time refusals
//! the corpus cannot carry because the documents are checker-legal, pinned
//! two-sided against the reference emitter on identical sources.

use super::{LowerError, lower};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A declared type whose snake stem coincides with a generated composite
/// stem: `ListString` and the `[String]` composite both stem `list_string`.
const COLLIDING: &str = r"//! Stem collision probe.
workflow collide_demo
  input items: [String]
  outcome kept: type ListString, route success

type ListString { first: String }

worker keeper
  action keep(items: [String]) -> ListString

step keep_step
  items |> keep |> route kept
";

/// A required-recursive record reaching the union `decode.failure` default
/// (S5): checker-legal, refused by BOTH sides.
const RECURSIVE: &str = r"//! Required-recursion probe.
workflow chain_demo
  input note: String
  outcome linked: type Chain, route success

type Chain { next: Chain, note: String }

worker linker
  action link(note: String) -> Chain

step link_step
  note |> link |> route linked
";

/// Panel blocker (S1 class): a duplicate codec stem must be a compile-time
/// refusal naming both parties — never two trios silently cross-wired through
/// one registry entry into validator-clean, runtime-wrong BEAM. Two-sided:
/// the reference emitter renders BOTH `fn list_string_codec` definitions on
/// the same document (its loud failure arrives downstream at `gleam build`);
/// the direct path refuses at lower time.
#[test]
fn colliding_codec_stems_are_refused_not_cross_wired() -> TestResult {
    let document = crate::parse(COLLIDING)?;
    let Err(error) = lower(&document, None) else {
        return Err("a codec stem collision must refuse at lower time".into());
    };
    let LowerError::Message { message, .. } = &error else {
        return Err(format!("the collision must be a hard `Message` error: {error:?}").into());
    };
    assert!(
        message.contains("codec name collision"),
        "diagnostic class drifted: {message}"
    );
    assert!(
        message.contains("`ListString`") && message.contains("List(String)"),
        "the diagnostic must name BOTH colliding parties: {message}"
    );
    assert!(
        message.contains("`list_string`"),
        "the diagnostic must name the shared stem: {message}"
    );
    // Reference side of the pin: emit accepts the same document and renders
    // the duplicate definitions `gleam build` will refuse.
    let generated = crate::emit(&document)?;
    assert_eq!(
        generated.matches("fn list_string_codec(").count(),
        2,
        "the reference emitter's duplicate-definition rendering drifted:\n{generated}"
    );
    Ok(())
}

/// Panel minor (S5): the required-recursive default refusal, previously
/// implemented on both sides but pinned by NO test on either. Both the
/// direct lowering and the reference emitter must refuse the identical
/// checker-legal document with the same diagnostic class.
#[test]
fn required_recursive_default_is_refused_on_both_sides() -> TestResult {
    const DIAGNOSTIC: &str = "type `Chain` recurses through required fields, \
                              so no default value exists for the generated decoder";
    let document = crate::parse(RECURSIVE)?;
    let Err(error) = lower(&document, None) else {
        return Err("a required-recursive default must refuse at lower time".into());
    };
    let LowerError::Message { message, .. } = &error else {
        return Err(format!("the recursion refusal must be a `Message` error: {error:?}").into());
    };
    assert_eq!(
        message, DIAGNOSTIC,
        "the direct diagnostic drifted from the reference class"
    );
    let emit_error = crate::emit(&document)
        .err()
        .ok_or("the reference emitter must refuse the same document")?;
    assert!(
        emit_error.to_string().contains(DIAGNOSTIC),
        "the reference diagnostic drifted: {emit_error}"
    );
    Ok(())
}
