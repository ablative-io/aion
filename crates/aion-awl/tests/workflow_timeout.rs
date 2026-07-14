//! Regression coverage for document-level workflow timeout metadata.

use std::path::Path;
use std::time::Duration;

use aion_awl::{check, compile, emit, parse, print};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SOURCE: &str = include_str!("fixtures/rev2/dag-fork/valid/after_single.awl");

fn with_timeout(duration: &str) -> String {
    SOURCE.replacen(
        "workflow after_single\n",
        &format!("workflow after_single\n  timeout {duration}\n"),
        1,
    )
}

#[test]
fn workflow_timeout_parses_and_prints_with_header_trivia() -> TestResult {
    let source = SOURCE.replacen(
        "workflow after_single\n",
        "workflow after_single\n  // package policy\n  timeout 6h // whole run\n",
        1,
    );
    let document = parse(&source)?;
    let timeout = document.timeout.as_ref().ok_or("timeout not parsed")?;
    assert_eq!(timeout.duration.magnitude, 6);
    assert_eq!(timeout.duration.span.line, 4);
    assert!(!timeout.negative);

    let canonical = print(&document);
    assert!(canonical.contains("  // package policy\n  timeout 6h // whole run\n"));
    assert_eq!(parse(&canonical)?, document);
    assert_eq!(print(&parse(&canonical)?), canonical);
    Ok(())
}

#[test]
fn zero_and_negative_workflow_timeouts_are_checked_at_the_declaration() -> TestResult {
    for duration in ["0s", "-5m"] {
        let document = parse(&with_timeout(duration))?;
        let errors = check(&document);
        assert_eq!(errors.len(), 1, "unexpected diagnostics for {duration}");
        assert_eq!(
            errors[0].message,
            "workflow `timeout` must be a positive duration"
        );
        assert_eq!(errors[0].span.line, 3);
        assert_eq!(errors[0].span.column, 3);
    }
    Ok(())
}

#[test]
fn overflowing_workflow_timeout_is_checked_at_the_declaration() -> TestResult {
    let document = parse(&with_timeout("18446744073709551615d"))?;
    let errors = check(&document);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].message, "workflow `timeout` is too large");
    assert_eq!(errors[0].span.line, 3);
    assert_eq!(errors[0].span.column, 3);

    let result = compile(&with_timeout("18446744073709551615d"), Path::new("."));
    assert!(matches!(
        result,
        Err(aion_awl::CompileError::Check(errors))
            if errors.iter().any(|error| error.message == "workflow `timeout` is too large")
    ));
    Ok(())
}

#[test]
fn timeout_is_compile_metadata_for_both_backends_not_workflow_code() -> TestResult {
    let plain_document = parse(SOURCE)?;
    let declared_source = with_timeout("6h");
    let declared_document = parse(&declared_source)?;

    assert_eq!(emit(&plain_document)?, emit(&declared_document)?);

    let plain = compile(SOURCE, Path::new("."))?;
    let declared = compile(&declared_source, Path::new("."))?;
    assert_eq!(plain.timeout, None);
    assert_eq!(declared.timeout, Some(Duration::from_secs(21_600)));
    assert_eq!(plain.beam_bytes, declared.beam_bytes);
    assert_eq!(plain.sidecar_bytes, declared.sidecar_bytes);
    Ok(())
}
