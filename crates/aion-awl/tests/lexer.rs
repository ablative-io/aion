//! Integration tests for the AWL rev-2 lexer: corpus coverage, exact flagship
//! token sequences, doc-line data tokens, and the keyword inventory.
//! Span discipline and error diagnostics live in `lexer_spans.rs`.

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use aion_awl::{Keyword, Token, TokenKind, lex};

type TestResult = Result<(), Box<dyn Error>>;

fn token_kinds(tokens: &[Token]) -> Vec<TokenKind> {
    tokens.iter().map(|token| token.kind.clone()).collect()
}

fn ident(name: &str) -> TokenKind {
    TokenKind::Identifier(name.to_owned())
}

fn type_ident(name: &str) -> TokenKind {
    TokenKind::TypeIdentifier(name.to_owned())
}

fn accessor(name: &str) -> TokenKind {
    TokenKind::FieldAccessor(name.to_owned())
}

fn kw(keyword: Keyword) -> TokenKind {
    TokenKind::Keyword(keyword)
}

// ---------------------------------------------------------------------
// rev-2 golden corpus: every fixture lexes
// ---------------------------------------------------------------------

fn collect_awl_files(dir: &Path, into: &mut Vec<PathBuf>) -> TestResult {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_awl_files(&path, into)?;
        } else if path.extension().is_some_and(|ext| ext == "awl") {
            into.push(path);
        }
    }
    Ok(())
}

/// Every corpus fixture — valid AND invalid — must lex cleanly: the sidecar
/// stages are PARSE and CHECK only, so no fixture is allowed to die in the
/// lexer.
#[test]
fn entire_rev2_corpus_lexes_without_error() -> TestResult {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rev2");
    let mut files = Vec::new();
    collect_awl_files(&root, &mut files)?;
    // 164 .awl fixtures after the 2026-07-11 ratified-rulings additions
    // (159 post-integration + 5 ruling fixtures); a lower count means
    // silent corpus loss, not a wrong root.
    assert!(
        files.len() >= 164,
        "corpus walk found only {} fixtures — corpus lost files or wrong root?",
        files.len()
    );

    let mut failures = Vec::new();
    for path in &files {
        let source = fs::read_to_string(path)?;
        if let Err(error) = lex(&source) {
            failures.push(format!(
                "{}: {} (line {}, column {})",
                path.display(),
                error.message,
                error.span.line,
                error.span.column
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "fixtures failed to lex:\n{}",
        failures.join("\n")
    );
    Ok(())
}

// ---------------------------------------------------------------------
// flagship fixture: exact token sequences
// ---------------------------------------------------------------------

#[test]
fn awl_hello_flagship_lexes_to_the_exact_token_sequence() -> TestResult {
    let source = include_str!("fixtures/rev2/flagship/valid/awl_hello.awl");
    let kinds = token_kinds(&lex(source)?);

    let expected = vec![
        TokenKind::DocHeader(
            " Greet a name, then shout it — the first workflow written in AWL and run for real."
                .to_owned(),
        ),
        TokenKind::Newline,
        kw(Keyword::Workflow),
        ident("awl_hello"),
        TokenKind::Newline,
        TokenKind::Indent,
        kw(Keyword::Input),
        ident("name"),
        TokenKind::Colon,
        type_ident("String"),
        TokenKind::Newline,
        kw(Keyword::Outcome),
        ident("shouted"),
        TokenKind::Colon,
        kw(Keyword::Type),
        type_ident("Shouted"),
        TokenKind::Comma,
        kw(Keyword::Route),
        kw(Keyword::Success),
        TokenKind::Newline,
        TokenKind::Dedent,
        kw(Keyword::Type),
        type_ident("Greeting"),
        TokenKind::LeftBrace,
        ident("greeting"),
        TokenKind::Colon,
        type_ident("String"),
        TokenKind::RightBrace,
        TokenKind::Newline,
        kw(Keyword::Type),
        type_ident("Shouted"),
        TokenKind::LeftBrace,
        ident("text"),
        TokenKind::Colon,
        type_ident("String"),
        TokenKind::RightBrace,
        TokenKind::Newline,
        kw(Keyword::Worker),
        ident("awl_hello"),
        TokenKind::Newline,
        TokenKind::Indent,
        kw(Keyword::Action),
        ident("greet"),
        TokenKind::LeftParen,
        ident("name"),
        TokenKind::Colon,
        type_ident("String"),
        TokenKind::RightParen,
        TokenKind::Arrow,
        type_ident("Greeting"),
        TokenKind::Newline,
        kw(Keyword::Action),
        ident("shout"),
        TokenKind::LeftParen,
        ident("text"),
        TokenKind::Colon,
        type_ident("String"),
        TokenKind::RightParen,
        TokenKind::Arrow,
        type_ident("Shouted"),
        TokenKind::Newline,
        TokenKind::Dedent,
        kw(Keyword::Step),
        ident("greet_and_shout"),
        TokenKind::Newline,
        TokenKind::Indent,
        ident("name"),
        TokenKind::Pipe,
        ident("greet"),
        TokenKind::Pipe,
        accessor("greeting"),
        TokenKind::Pipe,
        ident("shout"),
        TokenKind::Pipe,
        kw(Keyword::Route),
        ident("shouted"),
        TokenKind::Newline,
        TokenKind::Dedent,
    ];
    assert_eq!(kinds, expected);
    Ok(())
}

#[test]
fn dev_brief_loop_region_lexes_loop_counting_until_max() -> TestResult {
    let source = include_str!("fixtures/rev2/flagship/valid/dev_brief.awl");
    let tokens = lex(source)?;
    let kinds = token_kinds(&tokens);

    let start = kinds
        .iter()
        .position(|kind| *kind == kw(Keyword::Loop))
        .ok_or_else(|| io::Error::other("missing loop keyword"))?;

    let loop_header = &kinds[start..start + 16];
    assert_eq!(
        loop_header,
        [
            kw(Keyword::Loop),
            ident("round"),
            TokenKind::Equal,
            type_ident("Round"),
            TokenKind::LeftParen,
            ident("summary"),
            TokenKind::Colon,
            TokenKind::String(String::new()),
            TokenKind::Comma,
            ident("gates_green"),
            TokenKind::Colon,
            kw(Keyword::False),
            TokenKind::RightParen,
            kw(Keyword::Counting),
            ident("cycles"),
            TokenKind::Newline,
        ]
    );

    let until = kinds
        .iter()
        .position(|kind| *kind == kw(Keyword::Until))
        .ok_or_else(|| io::Error::other("missing until keyword"))?;
    assert_eq!(
        &kinds[until..until + 4],
        [
            kw(Keyword::Until),
            ident("round"),
            accessor("gates_green"),
            TokenKind::Newline,
        ]
    );

    let max = kinds
        .iter()
        .position(|kind| *kind == kw(Keyword::Max))
        .ok_or_else(|| io::Error::other("missing max keyword"))?;
    assert_eq!(
        &kinds[max..max + 4],
        [
            kw(Keyword::Max),
            ident("config"),
            accessor("max_fix_cycles"),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

// ---------------------------------------------------------------------
// doc lines are data tokens; `//` stays trivia
// ---------------------------------------------------------------------

#[test]
fn doc_header_lines_lex_as_data_with_verbatim_text() -> TestResult {
    let kinds = token_kinds(&lex("//! First narration line.\n//!second, no space\n")?);
    assert_eq!(
        kinds,
        vec![
            TokenKind::DocHeader(" First narration line.".to_owned()),
            TokenKind::Newline,
            TokenKind::DocHeader("second, no space".to_owned()),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn doc_lines_lex_as_data_and_do_not_disturb_indentation() -> TestResult {
    let source =
        "worker mailer\n  /// Deliver one message.\n  action broadcast(body: String) -> Nil\n";
    let kinds = token_kinds(&lex(source)?);
    assert_eq!(
        kinds,
        vec![
            TokenKind::Keyword(Keyword::Worker),
            ident("mailer"),
            TokenKind::Newline,
            TokenKind::DocLine(" Deliver one message.".to_owned()),
            TokenKind::Newline,
            TokenKind::Indent,
            TokenKind::Keyword(Keyword::Action),
            ident("broadcast"),
            TokenKind::LeftParen,
            ident("body"),
            TokenKind::Colon,
            type_ident("String"),
            TokenKind::RightParen,
            TokenKind::Arrow,
            type_ident("Nil"),
            TokenKind::Newline,
            TokenKind::Dedent,
        ]
    );
    Ok(())
}

#[test]
fn doc_line_spans_carry_true_position_and_marker() -> TestResult {
    let source = "type Section {\n  title: String,\n  /// Present only when trimmed.\n  trimmed_note: String?,\n}\n";
    let tokens = lex(source)?;
    let doc = tokens
        .iter()
        .find(|token| matches!(token.kind, TokenKind::DocLine(_)))
        .ok_or_else(|| io::Error::other("missing doc line token"))?;
    let expected_start = source
        .find("/// Present")
        .ok_or_else(|| io::Error::other("doc line present in source"))?;
    assert_eq!(doc.span.start, expected_start);
    assert_eq!(doc.span.line, 3);
    assert_eq!(doc.span.column, 3);
    Ok(())
}

#[test]
fn four_slashes_and_plain_comments_stay_trivia() -> TestResult {
    let kinds = token_kinds(&lex("//// not a doc line\n// plain trivia\n")?);
    assert_eq!(
        kinds,
        vec![
            TokenKind::Comment("// not a doc line".to_owned()),
            TokenKind::Comment("plain trivia".to_owned()),
        ]
    );
    Ok(())
}

#[test]
fn trailing_comment_after_code_stays_trivia_before_newline() -> TestResult {
    let kinds = token_kinds(&lex("step ship // fall-through\n")?);
    assert_eq!(
        kinds,
        vec![
            TokenKind::Keyword(Keyword::Step),
            ident("ship"),
            TokenKind::Comment("fall-through".to_owned()),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn trailing_triple_slash_after_code_is_trivia_not_doc_data() -> TestResult {
    // Doc-line classification is whole-line only (the spec defines `///` doc
    // LINES); a marker trailing code must not become a DocLine data token
    // that would misattach to the NEXT declaration.
    let kinds = token_kinds(&lex("step ship /// looks like a doc line\n")?);
    assert_eq!(
        kinds,
        vec![
            TokenKind::Keyword(Keyword::Step),
            ident("ship"),
            TokenKind::Comment("/ looks like a doc line".to_owned()),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn trailing_doc_header_marker_after_code_is_trivia_not_doc_data() -> TestResult {
    // `//!` is workflow narration only as a whole line before the header; a
    // trailing marker mid-file is an ordinary comment.
    let kinds = token_kinds(&lex("step ship //! narration tail\n")?);
    assert_eq!(
        kinds,
        vec![
            TokenKind::Keyword(Keyword::Step),
            ident("ship"),
            TokenKind::Comment("! narration tail".to_owned()),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

// ---------------------------------------------------------------------
// keyword inventory and dead words
// ---------------------------------------------------------------------

#[test]
fn every_rev2_keyword_lexes_as_a_keyword() -> TestResult {
    let inventory: &[(&str, Keyword)] = &[
        ("workflow", Keyword::Workflow),
        ("input", Keyword::Input),
        ("signal", Keyword::Signal),
        ("outcome", Keyword::Outcome),
        ("type", Keyword::Type),
        ("schema", Keyword::Schema),
        ("worker", Keyword::Worker),
        ("action", Keyword::Action),
        ("child", Keyword::Child),
        ("step", Keyword::Step),
        ("after", Keyword::After),
        ("fork", Keyword::Fork),
        ("join", Keyword::Join),
        ("loop", Keyword::Loop),
        ("counting", Keyword::Counting),
        ("until", Keyword::Until),
        ("max", Keyword::Max),
        ("sequential", Keyword::Sequential),
        ("spawn", Keyword::Spawn),
        ("wait", Keyword::Wait),
        ("sleep", Keyword::Sleep),
        ("timeout", Keyword::Timeout),
        ("retry", Keyword::Retry),
        ("every", Keyword::Every),
        ("backoff", Keyword::Backoff),
        ("node", Keyword::Node),
        ("on", Keyword::On),
        ("failure", Keyword::Failure),
        ("when", Keyword::When),
        ("otherwise", Keyword::Otherwise),
        ("route", Keyword::Route),
        ("success", Keyword::Success),
        ("filter", Keyword::Filter),
        ("map", Keyword::Map),
        ("any", Keyword::Any),
        ("all", Keyword::All),
        ("sort", Keyword::Sort),
        ("count", Keyword::Count),
        ("is", Keyword::Is),
        ("empty", Keyword::Empty),
        ("present", Keyword::Present),
        ("absent", Keyword::Absent),
        ("not", Keyword::Not),
        ("and", Keyword::And),
        ("or", Keyword::Or),
        ("true", Keyword::True),
        ("false", Keyword::False),
    ];
    for (word, keyword) in inventory {
        let tokens = lex(&format!("{word}\n"))?;
        assert_eq!(
            tokens[0].kind,
            TokenKind::Keyword(*keyword),
            "`{word}` must lex as a keyword"
        );
    }
    Ok(())
}

#[test]
fn dead_awl0_words_lex_as_plain_identifiers() -> TestResult {
    // Rejection of these is a parse-level migration diagnostic; the lexer
    // must not special-case them.
    let dead = [
        "do", "as", "each", "repeat", "finish", "match", "case", "parallel", "race", "output",
        "error", "about", "up", "to", "in", "order", "queue", "fail",
    ];
    for word in dead {
        let tokens = lex(&format!("{word}\n"))?;
        assert_eq!(
            tokens[0].kind,
            ident(word),
            "dead word `{word}` must lex as an identifier"
        );
    }
    Ok(())
}

#[test]
fn keywords_embedded_in_longer_identifiers_stay_identifiers() -> TestResult {
    let tokens = lex("max_fix_cycles counter workflow_id\n")?;
    let kinds = token_kinds(&tokens);
    assert_eq!(
        &kinds[..3],
        [
            ident("max_fix_cycles"),
            ident("counter"),
            ident("workflow_id"),
        ]
    );
    Ok(())
}
