//! Integration tests for AWL parsing, canonical printing, and diagnostics.

use std::error::Error;

use aion_awl::{StepOp, parse, print};

fn assert_idempotent(source: &str) -> Result<(), Box<dyn Error>> {
    let first = print(&parse(source)?);
    let second = print(&parse(&first)?);
    assert_eq!(second, first);
    Ok(())
}

fn debug_without_spans<T: std::fmt::Debug>(value: &T) -> String {
    let debug = format!("{value:#?}");
    let mut out = String::new();
    let mut rest = debug.as_str();
    while let Some(start) = rest.find("span: Span {") {
        out.push_str(&rest[..start]);
        out.push_str("span: _");
        let after = &rest[start..];
        let Some(end) = after.find('}') else {
            break;
        };
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[test]
fn research_report_normalizes_to_golden_and_preserves_comments() -> Result<(), Box<dyn Error>> {
    let source = include_str!("fixtures/research_report.awl");
    let canonical = include_str!("fixtures/research_report.canonical.awl");
    let normalized_source = print(&parse(source)?);
    assert_eq!(
        debug_without_spans(&parse(&normalized_source)?),
        debug_without_spans(&parse(canonical)?)
    );
    let printed = normalized_source;
    assert_eq!(printed, canonical);
    assert_eq!(print(&parse(canonical)?), canonical);
    assert_eq!(print(&parse(&printed)?), printed);
    for comment in [
        "structured — parsed + validated from brief.json at start",
        "bulk — content-addressed snapshot handle (haematite)",
        "rebinds — every later step sees the revised draft",
    ] {
        assert!(canonical.contains(comment), "missing comment {comment}");
    }
    Ok(())
}

#[test]
fn bounded_cycle_normalizes_to_golden_and_is_idempotent() -> Result<(), Box<dyn Error>> {
    let source = include_str!("fixtures/bounded_cycle.awl");
    let canonical = include_str!("fixtures/bounded_cycle.canonical.awl");
    let normalized_source = print(&parse(source)?);
    assert_eq!(
        debug_without_spans(&parse(&normalized_source)?),
        debug_without_spans(&parse(canonical)?)
    );
    assert_eq!(print(&parse(source)?), canonical);
    assert_eq!(print(&parse(canonical)?), canonical);
    assert_idempotent(source)
}

#[test]
fn typed_contract_normalizes_to_golden_and_is_idempotent() -> Result<(), Box<dyn Error>> {
    let source = include_str!("fixtures/typed_contract.awl");
    let canonical = include_str!("fixtures/typed_contract.canonical.awl");
    assert_eq!(print(&parse(source)?), canonical);
    assert_eq!(print(&parse(canonical)?), canonical);
    assert_idempotent(source)
}

#[test]
fn described_type_fields_are_load_bearing_data() -> Result<(), Box<dyn Error>> {
    let document = parse(include_str!("fixtures/typed_contract.awl"))?;
    let brief = document
        .types
        .iter()
        .find(|ty| ty.name == "Brief")
        .ok_or("Brief type")?;
    assert_eq!(
        brief.description.as_deref(),
        Some("A development brief emitted as a model output contract.")
    );
    assert_eq!(
        brief.fields[0].description.as_deref(),
        Some("Stable identifier, for example AWL1-001.")
    );
    assert_eq!(brief.fields[2].description, None);
    Ok(())
}

#[test]
fn type_comments_and_doc_spacing_round_trip_without_promotion() -> Result<(), Box<dyn Error>> {
    let source = "//// banner\nworkflow w\noutput Box\n\n///Box docs.\ntype Box {\n  // field trivia\n  ///Value docs.\n  value: String\n}\n\naction make() -> Box\n\nstep make\n  do make()\n  as value\n\nfinish value\n";
    let printed = print(&parse(source)?);
    assert!(printed.contains("// // banner\nworkflow w"));
    assert!(printed.contains("///Box docs.\ntype Box"));
    assert!(printed.contains("  // field trivia\n  ///Value docs.\n  value: String,"));
    let document = parse(&printed)?;
    let declaration = document.types.first().ok_or("Box type")?;
    assert_eq!(declaration.description.as_deref(), Some("Box docs."));
    assert_eq!(
        declaration.fields[0].description.as_deref(),
        Some("Value docs.")
    );
    assert_eq!(declaration.fields[0].trivia.leading.len(), 1);
    Ok(())
}

#[test]
fn ordinary_comment_before_documented_type_survives_fmt() -> Result<(), Box<dyn Error>> {
    // An ordinary `//` comment interleaved directly before a `///`-documented
    // type must survive the round-trip byte-for-byte. The description machinery
    // consumes the doc comment when attaching it to the type; it must not also
    // drop the ordinary comment sitting just above it.
    let source = "workflow w\n\noutput Box\n\n// ordinary before type\n/// Box docs.\ntype Box { value: String }\nfinish Box(value: \"x\")\n";
    let printed = print(&parse(source)?);
    assert_eq!(printed, source, "canonical form is not a fixed point");
    assert!(
        printed.contains("// ordinary before type\n/// Box docs.\ntype Box"),
        "ordinary comment before the documented type was lost: {printed}"
    );
    let document = parse(source)?;
    let declaration = document.types.first().ok_or("Box type")?;
    assert_eq!(declaration.description.as_deref(), Some("Box docs."));
    assert_idempotent(source)
}

#[test]
fn orphan_doc_comment_before_workflow_round_trips_unmangled() -> Result<(), Box<dyn Error>> {
    // A `///` doc comment that precedes a construct which cannot carry a
    // description (here `workflow`) attaches to nothing and is preserved as
    // trivia. It must re-render as a well-formed `///` line, never be sliced
    // into a `// /` ordinary comment.
    let source = "/// orphan before workflow\nworkflow w\n\noutput String\n\nfinish \"x\"\n";
    let expected = "/// orphan before workflow\nworkflow w\n\noutput String\nfinish \"x\"\n";
    let printed = print(&parse(source)?);
    assert_eq!(printed, expected);
    assert!(
        printed.starts_with("/// orphan before workflow\n"),
        "orphan doc comment was corrupted: {printed}"
    );
    assert!(
        !printed.contains("// /"),
        "doc comment was mangled through the ordinary-comment path: {printed}"
    );
    assert_idempotent(source)
}

#[test]
fn probe_round_trips_without_comment_loss_or_mangling() -> Result<(), Box<dyn Error>> {
    // The full corruption probe: an orphan `///` before `workflow`, and an
    // ordinary `//` interleaved before a `///`-documented type. Neither
    // comment may be dropped nor mangled, and the canonical form is stable.
    let source = "/// orphan before workflow\nworkflow w\noutput Box\n\n// ordinary before type\n/// Box docs.\ntype Box { value: String }\n\nfinish Box(value: \"x\")\n";
    let expected = "/// orphan before workflow\nworkflow w\n\noutput Box\n\n// ordinary before type\n/// Box docs.\ntype Box { value: String }\nfinish Box(value: \"x\")\n";
    let printed = print(&parse(source)?);
    assert_eq!(printed, expected);
    assert!(printed.contains("/// orphan before workflow\n"));
    assert!(printed.contains("// ordinary before type\n"));
    assert!(!printed.contains("// /"), "mangled doc comment: {printed}");
    assert_idempotent(source)
}

#[test]
fn field_docs_force_multi_line_even_when_single_line_would_fit() -> Result<(), Box<dyn Error>> {
    // Operator ruling on AWL1-001: the single-line form has nowhere to carry
    // per-field `///` docs, so a documented field forces the multi-line
    // rendering even for a declaration well under the 100-column boundary.
    // Collapsing would silently discard the field's load-bearing prose.
    let source = "workflow w\n\noutput Box\n\ntype Box {\n  /// Value docs.\n  value: String,\n}\nfinish Box(value: \"x\")\n";
    let printed = print(&parse(source)?);
    assert_eq!(printed, source, "documented field must not collapse");
    assert!(
        printed.contains("type Box {\n  /// Value docs.\n  value: String,\n}"),
        "multi-line form with the field doc expected: {printed}"
    );
    assert_idempotent(source)
}

#[test]
fn type_printer_uses_the_inclusive_100_column_boundary() -> Result<(), Box<dyn Error>> {
    let at_limit = "a".repeat(81);
    let over_limit = "a".repeat(82);
    let source_at_limit = format!(
        "workflow w\noutput String\n\ntype T {{ {at_limit}: String }}\n\nfinish \"done\"\n"
    );
    let source_over_limit = format!(
        "workflow w\noutput String\n\ntype T {{ {over_limit}: String }}\n\nfinish \"done\"\n"
    );
    let printed_at_limit = print(&parse(&source_at_limit)?);
    let type_line = printed_at_limit
        .lines()
        .find(|line| line.starts_with("type T"))
        .ok_or("type line")?;
    assert_eq!(type_line.chars().count(), 100);
    assert!(type_line.ends_with(" }"));
    let printed_over_limit = print(&parse(&source_over_limit)?);
    assert!(printed_over_limit.contains("type T {\n  "));
    assert!(printed_over_limit.contains(": String,\n}"));
    Ok(())
}

#[test]
fn differently_formatted_same_document_print_identically() -> Result<(), Box<dyn Error>> {
    let fixture = include_str!("fixtures/research_report.awl");
    let variant = fixture
        .replace("input brief: Brief      //", "input brief: Brief //")
        .replace("\n\ninput corpus", "\n\n\ninput corpus")
        .replace("as draft                       //", "as draft //");
    assert_eq!(print(&parse(fixture)?), print(&parse(&variant)?));
    Ok(())
}

#[test]
fn mutating_ast_changes_printed_output() -> Result<(), Box<dyn Error>> {
    let mut document = parse(include_str!("fixtures/bounded_cycle.awl"))?;
    let step = document
        .steps
        .iter_mut()
        .find(|step| step.name == "fix")
        .ok_or("fix step present")?;
    step.name = "repair".to_owned();
    if let StepOp::Do(_) = &step.op {
        let retry = step.retry.as_mut().ok_or("retry present on fix step")?;
        if let aion_awl::RetrySpec::Backoff { min, .. } = retry {
            min.magnitude = 10;
        }
    }
    let printed = print(&document);
    assert!(printed.contains("step repair"));
    assert!(printed.contains("retry 2 backoff 10s..1m"));
    Ok(())
}

#[test]
fn printer_is_idempotent_for_noncanonical_variants() -> Result<(), Box<dyn Error>> {
    let variants = [
        "workflow w\nabout x\n\ninput a: String\noutput String\n\naction make(a: String) -> String\n\nstep one\n  when true or false and not false\n  do make(a)\n  as out\n\nfinish out\n",
        "workflow w\ninput a: String   // c\noutput String\n\ntype Pair { left: String, right: String }\naction make(a: String) -> Pair\n\nstep one\n  do make(a)\n  as p\n\nfinish Pair(left: p.left, right: \"x\")\n",
        "workflow w\ninput a: String\noutput String\n\nsignal done: String\naction make(a: String) -> String\n\nstep one\n  wait done\n  timeout 1h\n  on timeout\n    finish \"timeout\"\n  as out\n\nfinish out\n",
    ];
    for variant in variants {
        assert_idempotent(variant)?;
    }
    Ok(())
}

#[test]
fn parse_errors_are_spanned_and_specific() -> Result<(), Box<dyn Error>> {
    // (source, expected message substring, expected line, expected column, expected byte start)
    let cases = [
        (
            // Second op keyword ("wait" on line 6) collides with "do" already
            // set on line 5, so the error points at the "wait sig" line: byte
            // offset 44 is the 'w' of "wait", the 3rd column of the 6th line.
            "workflow w\noutput String\n\nstep x\n  do a()\n  wait sig\n\nfinish ok\n",
            "exactly one",
            6,
            3,
            44,
        ),
        (
            // No `finish` line at all, so the error falls back to the span of
            // the last parsed source line ("output String", line 2), whose
            // code starts right after the 11-byte first line at offset 11.
            "workflow w\noutput String\n",
            "missing finish",
            2,
            1,
            11,
        ),
        (
            // The nested call `b()` inside `do a(b())` is rejected at the `b`
            // identifier token: line 5 column 8, byte offset 40 (the do-line
            // starts at byte 33, "  do a(" occupies 7 of those bytes).
            "workflow w\noutput String\n\nstep x\n  do a(b())\n\nfinish ok\n",
            "call expressions",
            5,
            8,
            40,
        ),
        (
            // Unknown step field `frob` spans its whole line: line 5, column
            // 3 (after the 2-space indent), byte offset 35.
            "workflow w\noutput String\n\nstep x\n  frob yes\n\nfinish ok\n",
            "unknown step field",
            5,
            3,
            35,
        ),
        (
            // `finish Out(a: 1` runs out of tokens before the closing `)` or
            // a `,`, so the error falls back to the expression context, which
            // starts at the `O` of `Out`: line 7, column 8, byte offset 50.
            "workflow w\noutput String\n\nstep x\n  do a()\n\nfinish Out(a: 1\n",
            "unterminated record",
            7,
            8,
            50,
        ),
        (
            // `on failure` demands a 4-space-indented handler body next, but
            // `  fail` is only indented 2, so the error points at that line:
            // line 7, column 3, byte offset 57.
            "workflow w\noutput String\n\nstep x\n  do a()\n  on failure\n  fail\n\nfinish ok\n",
            "wrong indentation",
            7,
            3,
            57,
        ),
    ];
    for (source, expected, line, column, start) in cases {
        let Err(error) = parse(source) else {
            return Err("diagnostic fixture should be rejected".into());
        };
        assert!(
            error.message.contains(expected),
            "{expected:?} not in {:?}",
            error.message
        );
        assert_eq!(error.span.line, line, "line mismatch for {expected:?}");
        assert_eq!(
            error.span.column, column,
            "column mismatch for {expected:?}"
        );
        assert_eq!(error.span.start, start, "start mismatch for {expected:?}");
    }
    Ok(())
}
