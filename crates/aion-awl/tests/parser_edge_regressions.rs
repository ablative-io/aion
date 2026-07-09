//! Edge-case regression tests for the AWL parser's classification/extraction
//! boundary, guarding the salvage fixes made when `parser.rs` was split into
//! the `parser/` folder module and its bypass lints were purged.
//!
//! The purge replaced `first_word(...) == "<kw>"` classification followed by a
//! byte-level `strip_prefix("<kw>").unwrap()` extraction (a panic on mismatch)
//! with fallible extraction. `first_word` splits on Unicode whitespace while
//! `strip_prefix` matches raw bytes, so a leading no-break space (U+00A0)
//! before a keyword makes the two disagree: the word splitter sees the keyword
//! but the byte prefix is absent. The old code panicked on that input; the new
//! code must surface an explicit `ParseError` and must never bump-and-drop the
//! line silently.
//!
//! These tests deliberately carry no lint-bypass attributes and use no
//! `unwrap`/`expect`/`panic!`: every fallible step propagates with `?` or a
//! `let`-`else` that returns an explanatory error.

use std::error::Error;

use aion_awl::{ParseError, parse, print};

/// Parse `source`, requiring it to fail, and return the resulting error.
fn expect_parse_error(source: &str) -> Result<ParseError, Box<dyn Error>> {
    match parse(source) {
        Ok(_) => Err(format!("expected a parse error but parsing succeeded for {source:?}").into()),
        Err(err) => Ok(err),
    }
}

/// A no-break space before `about` at document level: `first_word` classifies
/// it as an `about` line, but byte-level extraction cannot strip the keyword.
/// The line must NOT be silently consumed — the document loop must reach it and
/// reject it with an explicit unknown-declaration diagnostic (the old parser
/// panicked here via `strip_prefix("about").unwrap()`).
#[test]
fn nbsp_before_about_is_rejected_not_silently_dropped() -> Result<(), Box<dyn Error>> {
    let err = expect_parse_error("workflow W\n\u{00a0}about hidden\nfinish true\n")?;
    assert_eq!(err.message, "unknown declaration `about`");
    // The diagnostic points at the offending second line, column 1.
    assert_eq!(err.span.line, 2);
    assert_eq!(err.span.column, 1);
    Ok(())
}

/// The same classification/extraction divergence for every other document-level
/// keyword must also surface an explicit `ParseError` rather than panicking or
/// silently consuming the line. Each of these went through
/// `strip_prefix("<kw>").unwrap()` in the old parser and now flows through the
/// fallible `keyword_rest` helper.
#[test]
fn nbsp_before_document_keywords_yields_explicit_errors() -> Result<(), Box<dyn Error>> {
    let cases = [
        (
            "workflow W\n\u{00a0}input x: Int\nfinish true\n",
            "IO declaration keyword mismatch",
        ),
        (
            "workflow W\n\u{00a0}finish true\n",
            "finish declaration needs an expression",
        ),
        (
            "workflow W\n\u{00a0}type T { a: Int }\nfinish true\n",
            "type declaration needs record fields",
        ),
        (
            "workflow W\n\u{00a0}action a() -> Int\nfinish true\n",
            "action declaration needs `-> ReturnType`",
        ),
    ];
    for (source, expected) in cases {
        let err = expect_parse_error(source)?;
        assert_eq!(err.message, expected, "wrong diagnostic for {source:?}");
    }
    Ok(())
}

/// A valid document-level `about` declaration must still parse and survive a
/// print round-trip byte-for-byte, proving the classification fix did not
/// perturb the accepted-input behavior.
#[test]
fn valid_about_round_trips() -> Result<(), Box<dyn Error>> {
    let doc = parse("workflow w\nabout does a thing\nfinish ok\n")?;
    let Some(about) = doc.about.as_ref() else {
        return Err("about declaration should be present".into());
    };
    assert_eq!(about.text, "does a thing");
    let printed = print(&doc);
    assert_eq!(
        print(&parse(&printed)?),
        printed,
        "print . parse . print must be idempotent"
    );
    Ok(())
}

/// The bare keyword `about` (no text) remains a valid empty-text declaration —
/// `strip_prefix("about")` yields the empty string, which is classified and
/// extracted, not rejected.
#[test]
fn bare_about_keyword_parses_with_empty_text() -> Result<(), Box<dyn Error>> {
    let doc = parse("workflow w\nabout\nfinish ok\n")?;
    let Some(about) = doc.about.as_ref() else {
        return Err("bare about should still be an about declaration".into());
    };
    assert_eq!(about.text, "");
    Ok(())
}

/// A token that merely starts with the bytes of a keyword but is not the whole
/// word (e.g. `aboutish`) must NOT be classified as an `about` declaration —
/// the word-boundary check mirrors the old `first_word` semantics, so the line
/// falls through to the unknown-declaration error.
#[test]
fn non_word_boundary_keyword_is_not_an_about() -> Result<(), Box<dyn Error>> {
    let err = expect_parse_error("workflow w\naboutish thing\nfinish ok\n")?;
    assert_eq!(err.message, "unknown declaration `aboutish`");
    Ok(())
}
