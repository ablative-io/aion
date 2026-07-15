//! Defense-in-depth regressions for checker-refused mixed-type concatenation.

use std::error::Error;
use std::path::Path;

use aion_awl::mir::{LowerError, lower, select};
use aion_awl::{CompileError, check_in, compile, parse};

type TestResult = Result<(), Box<dyn Error>>;

const INT_PLUS_STRING: &str = r#"//! Mixed concatenation must never reach selectable MIR.
workflow int_plus_string
  input total: Int
  outcome done: type Done, route success

type Done { value: String }

step finish
  route done(value: total + "x")
"#;

const STRING_PLUS_INT: &str = r#"//! Both concatenation operands must be strings.
workflow string_plus_int
  input total: Int
  outcome done: type Done, route success

type Done { value: String }

step finish
  route done(value: "x" + total)
"#;

const STRING_ALIAS_CONCAT: &str = r#"//! A scalar schema alias retains String concat semantics.
workflow string_alias_concat
  input prefix: Prefix
  outcome done: type Done, route success

type Prefix = schema {
  "type": "string"
}
type Done { value: String }

step finish
  route done(value: prefix + "/suffix")
"#;

const INT_ALIAS_PLUS_STRING: &str = r#"//! A scalar Int alias is not a string.
workflow int_alias_plus_string
  input total: Total
  outcome done: type Done, route success

type Total = schema {
  "type": "integer"
}
type Done { value: String }

step finish
  route done(value: total + "x")
"#;

// Public `lower` refuses documents that do not check cleanly at the door,
// so direct lowering of mixed concat now fails with the checker's own
// diagnostic (the lowering-internal `Unsupported { shape: "string
// concatenation" }` refusal remains behind the gate as defense in depth).
fn assert_lower_refuses(source: &str, expected_line: usize) -> TestResult {
    let document = parse(source)?;
    match lower(&document, None) {
        Err(LowerError::Message { message, span }) => {
            assert!(
                message.starts_with("document does not check cleanly: `+` joins strings only"),
                "unexpected refusal: {message}"
            );
            assert_eq!(span.line, expected_line);
            Ok(())
        }
        Err(other) => {
            Err(format!("mixed concat reached the wrong lowering refusal: {other}").into())
        }
        Ok(_) => Err("mixed concat lowered to selectable MIR".into()),
    }
}

#[test]
fn direct_lower_refuses_concat_unless_both_operands_are_strings() -> TestResult {
    for source in [INT_PLUS_STRING, STRING_PLUS_INT] {
        assert_lower_refuses(source, 9)?;
    }
    Ok(())
}

#[test]
fn checked_compile_rejects_mixed_concat_before_lowering() -> TestResult {
    let cases = [
        (INT_PLUS_STRING, "found Int and String"),
        (STRING_PLUS_INT, "found String and Int"),
    ];
    for (source, expected_types) in cases {
        match compile(source, Path::new(".")) {
            Err(CompileError::Check(errors)) => {
                assert!(
                    errors.iter().any(|error| {
                        error.message.starts_with(
                            "`+` joins strings only — arithmetic is not in the language",
                        ) && error.message.contains(expected_types)
                    }),
                    "checker diagnostics did not contain the established concat refusal: {errors:?}"
                );
            }
            Err(other) => {
                return Err(
                    format!("checker-refused concat escaped to a later stage: {other}").into(),
                );
            }
            Ok(_) => return Err("checker-refused concat compiled successfully".into()),
        }
    }
    Ok(())
}

#[test]
fn checker_approved_string_alias_concat_lowers_and_selects() -> TestResult {
    let document = parse(STRING_ALIAS_CONCAT)?;
    let errors = check_in(&document, Path::new("."));
    if !errors.is_empty() {
        return Err(format!("String alias concat no longer checks cleanly: {errors:?}").into());
    }
    let module = lower(&document, None)?;
    let beam = select(&module)?;
    assert!(
        beam.starts_with(b"FOR1"),
        "selection did not return BEAM bytes"
    );
    Ok(())
}

#[test]
fn int_alias_plus_string_still_refuses_at_direct_lowering() -> TestResult {
    let document = parse(INT_ALIAS_PLUS_STRING)?;
    let errors = check_in(&document, Path::new("."));
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("found Total and String")),
        "Int alias concat no longer carries the checker refusal: {errors:?}"
    );
    assert_lower_refuses(INT_ALIAS_PLUS_STRING, 12)
}
