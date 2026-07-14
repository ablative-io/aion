//! Defense-in-depth regressions for checker-refused mixed-type concatenation.

use std::error::Error;
use std::path::Path;

use aion_awl::mir::{LowerError, lower};
use aion_awl::{CompileError, compile, parse};

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

#[test]
fn direct_lower_refuses_concat_unless_both_operands_are_strings() -> TestResult {
    for source in [INT_PLUS_STRING, STRING_PLUS_INT] {
        let document = parse(source)?;
        match lower(&document, None) {
            Err(LowerError::Unsupported { shape, span }) => {
                assert_eq!(shape, "string concatenation");
                assert_eq!(span.line, 9);
            }
            Err(other) => {
                return Err(
                    format!("mixed concat reached the wrong lowering refusal: {other}").into(),
                );
            }
            Ok(_) => return Err("mixed concat lowered to selectable MIR".into()),
        }
    }
    Ok(())
}

#[test]
fn checked_compile_rejects_mixed_concat_before_lowering() -> TestResult {
    match compile(INT_PLUS_STRING, Path::new(".")) {
        Err(CompileError::Check(errors)) => {
            let expected =
                "`+` joins strings only — arithmetic is not in the language (found Int and String)";
            assert!(
                errors.iter().any(|error| error.message == expected),
                "checker diagnostics did not contain the established concat refusal: {errors:?}"
            );
            Ok(())
        }
        Err(other) => {
            Err(format!("checker-refused concat escaped to a later stage: {other}").into())
        }
        Ok(_) => Err("checker-refused concat compiled successfully".into()),
    }
}
