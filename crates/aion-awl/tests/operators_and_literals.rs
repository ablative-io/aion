//! Integration tests for AWL rev-2 operators and literal forms: `->`, `|>`,
//! `?`, `=`, `|`, comparisons, predicates, durations, lists, floats, strings,
//! and `.field` accessors.

use std::error::Error;

use aion_awl::{DurationUnit, Keyword, Token, TokenKind, lex};

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

#[test]
fn pipe_chain_with_accessor_stage_and_route_terminator() -> TestResult {
    let kinds = token_kinds(&lex(
        "name |> greet |> .greeting |> shout |> route shouted\n",
    )?);
    assert_eq!(
        kinds,
        vec![
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
        ]
    );
    Ok(())
}

#[test]
fn arrow_binds_call_results_and_accessors_chain() -> TestResult {
    let kinds = token_kinds(&lex(
        "provision(repo_root: config.repo_root) -> workspace\n",
    )?);
    assert_eq!(
        kinds,
        vec![
            ident("provision"),
            TokenKind::LeftParen,
            ident("repo_root"),
            TokenKind::Colon,
            ident("config"),
            accessor("repo_root"),
            TokenKind::RightParen,
            TokenKind::Arrow,
            ident("workspace"),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn enum_bar_is_distinct_from_pipe() -> TestResult {
    let kinds = token_kinds(&lex("type Category = Urgent | Routine | Spam\n")?);
    assert_eq!(
        kinds,
        vec![
            kw(Keyword::Type),
            type_ident("Category"),
            TokenKind::Equal,
            type_ident("Urgent"),
            TokenKind::Bar,
            type_ident("Routine"),
            TokenKind::Bar,
            type_ident("Spam"),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn question_marks_optional_types_including_list_types() -> TestResult {
    let kinds = token_kinds(&lex("reject_reason: String?\nextra_labels: [String]?\n")?);
    assert_eq!(
        kinds,
        vec![
            ident("reject_reason"),
            TokenKind::Colon,
            type_ident("String"),
            TokenKind::Question,
            TokenKind::Newline,
            ident("extra_labels"),
            TokenKind::Colon,
            TokenKind::LeftBracket,
            type_ident("String"),
            TokenKind::RightBracket,
            TokenKind::Question,
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn loop_seed_equal_and_schema_door_equal_lex_as_equal() -> TestResult {
    let kinds = token_kinds(&lex(
        "loop round = Round(summary: \"\") counting cycles\ntype Brief = schema(\"schemas/brief.schema.json\")\n",
    )?);
    assert_eq!(
        kinds,
        vec![
            kw(Keyword::Loop),
            ident("round"),
            TokenKind::Equal,
            type_ident("Round"),
            TokenKind::LeftParen,
            ident("summary"),
            TokenKind::Colon,
            TokenKind::String(String::new()),
            TokenKind::RightParen,
            kw(Keyword::Counting),
            ident("cycles"),
            TokenKind::Newline,
            kw(Keyword::Type),
            type_ident("Brief"),
            TokenKind::Equal,
            kw(Keyword::Schema),
            TokenKind::LeftParen,
            TokenKind::String("schemas/brief.schema.json".to_owned()),
            TokenKind::RightParen,
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn comparisons_booleans_and_string_concat_lex() -> TestResult {
    let kinds = token_kinds(&lex(
        "when not ok and total >= 3 or ratio < 1.5\nwhen cycles == 1\nwhen a != b, route x\nnote + \"!\"\nwhen n <= 2\nwhen n > 0\n",
    )?);
    assert_eq!(
        kinds,
        vec![
            kw(Keyword::When),
            kw(Keyword::Not),
            ident("ok"),
            kw(Keyword::And),
            ident("total"),
            TokenKind::GreaterEqual,
            TokenKind::Integer(3),
            kw(Keyword::Or),
            ident("ratio"),
            TokenKind::Less,
            TokenKind::Float("1.5".to_owned()),
            TokenKind::Newline,
            kw(Keyword::When),
            ident("cycles"),
            TokenKind::EqualEqual,
            TokenKind::Integer(1),
            TokenKind::Newline,
            kw(Keyword::When),
            ident("a"),
            TokenKind::BangEqual,
            ident("b"),
            TokenKind::Comma,
            kw(Keyword::Route),
            ident("x"),
            TokenKind::Newline,
            ident("note"),
            TokenKind::Plus,
            TokenKind::String("!".to_owned()),
            TokenKind::Newline,
            kw(Keyword::When),
            ident("n"),
            TokenKind::LessEqual,
            TokenKind::Integer(2),
            TokenKind::Newline,
            kw(Keyword::When),
            ident("n"),
            TokenKind::Greater,
            TokenKind::Integer(0),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn predicates_lex_as_keyword_pairs() -> TestResult {
    let kinds = token_kinds(&lex(
        "when blocking is empty\nwhen decision is present\nwhen note is absent\n",
    )?);
    assert_eq!(
        kinds,
        vec![
            kw(Keyword::When),
            ident("blocking"),
            kw(Keyword::Is),
            kw(Keyword::Empty),
            TokenKind::Newline,
            kw(Keyword::When),
            ident("decision"),
            kw(Keyword::Is),
            kw(Keyword::Present),
            TokenKind::Newline,
            kw(Keyword::When),
            ident("note"),
            kw(Keyword::Is),
            kw(Keyword::Absent),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn combinators_take_accessor_arguments() -> TestResult {
    let kinds = token_kinds(&lex("verdicts |> filter(.blocking) -> blocking\n")?);
    assert_eq!(
        kinds,
        vec![
            ident("verdicts"),
            TokenKind::Pipe,
            kw(Keyword::Filter),
            TokenKind::LeftParen,
            accessor("blocking"),
            TokenKind::RightParen,
            TokenKind::Arrow,
            ident("blocking"),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn durations_lex_with_all_units_and_backoff_ranges() -> TestResult {
    let kinds = token_kinds(&lex(
        "node shell, timeout 5m, retry 2 every 30s\nretry 5 backoff 10s..3d\nsleep 2h\n",
    )?);
    assert_eq!(
        kinds,
        vec![
            kw(Keyword::Node),
            ident("shell"),
            TokenKind::Comma,
            kw(Keyword::Timeout),
            TokenKind::Duration {
                magnitude: 5,
                unit: DurationUnit::Minutes,
            },
            TokenKind::Comma,
            kw(Keyword::Retry),
            TokenKind::Integer(2),
            kw(Keyword::Every),
            TokenKind::Duration {
                magnitude: 30,
                unit: DurationUnit::Seconds,
            },
            TokenKind::Newline,
            kw(Keyword::Retry),
            TokenKind::Integer(5),
            kw(Keyword::Backoff),
            TokenKind::Duration {
                magnitude: 10,
                unit: DurationUnit::Seconds,
            },
            TokenKind::DotDot,
            TokenKind::Duration {
                magnitude: 3,
                unit: DurationUnit::Days,
            },
            TokenKind::Newline,
            kw(Keyword::Sleep),
            TokenKind::Duration {
                magnitude: 2,
                unit: DurationUnit::Hours,
            },
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn separated_unit_letter_is_not_a_duration() -> TestResult {
    let kinds = token_kinds(&lex("sleep 30 s\n")?);
    assert_eq!(
        kinds,
        vec![
            kw(Keyword::Sleep),
            TokenKind::Integer(30),
            ident("s"),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn unit_prefixed_identifier_is_not_a_duration() -> TestResult {
    // `30s2` and `5min` must not split into duration + garbage.
    let kinds = token_kinds(&lex("wait 30s2\n")?);
    assert_eq!(
        kinds,
        vec![
            kw(Keyword::Wait),
            TokenKind::Integer(30),
            ident("s2"),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn list_literals_and_literal_indexing_lex() -> TestResult {
    let kinds = token_kinds(&lex(
        "broadcast(recipients: [\"ops\", \"oncall\"])\nroster[0]\n",
    )?);
    assert_eq!(
        kinds,
        vec![
            ident("broadcast"),
            TokenKind::LeftParen,
            ident("recipients"),
            TokenKind::Colon,
            TokenKind::LeftBracket,
            TokenKind::String("ops".to_owned()),
            TokenKind::Comma,
            TokenKind::String("oncall".to_owned()),
            TokenKind::RightBracket,
            TokenKind::RightParen,
            TokenKind::Newline,
            ident("roster"),
            TokenKind::LeftBracket,
            TokenKind::Integer(0),
            TokenKind::RightBracket,
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn string_escapes_booleans_and_floats_lex() -> TestResult {
    let kinds = token_kinds(&lex(
        "body: \"go \\\"now\\\"\\n\\t\\\\\", urgent: true, quiet: false, weight: 0.5\n",
    )?);
    assert_eq!(
        kinds,
        vec![
            ident("body"),
            TokenKind::Colon,
            TokenKind::String("go \"now\"\n\t\\".to_owned()),
            TokenKind::Comma,
            ident("urgent"),
            TokenKind::Colon,
            kw(Keyword::True),
            TokenKind::Comma,
            ident("quiet"),
            TokenKind::Colon,
            kw(Keyword::False),
            TokenKind::Comma,
            ident("weight"),
            TokenKind::Colon,
            TokenKind::Float("0.5".to_owned()),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn float_lexeme_is_preserved_verbatim() -> TestResult {
    let kinds = token_kinds(&lex("cap: 1.50\nlow: 0.10\n")?);
    assert!(kinds.contains(&TokenKind::Float("1.50".to_owned())));
    assert!(kinds.contains(&TokenKind::Float("0.10".to_owned())));
    Ok(())
}

#[test]
fn inline_schema_json_tokens_lex() -> TestResult {
    // Inline `schema { … }` bodies are raw-captured as a single SchemaBody
    // token (see tests/schema_door.rs) — the lexer never tokenizes schema
    // content. This test only pins that JSON-looking fragments OUTSIDE a
    // schema door still lex as ordinary AWL tokens.
    let kinds = token_kinds(&lex("  \"required\": [\"summary\"],\n  \"flag\": true\n")?);
    assert_eq!(
        kinds,
        vec![
            TokenKind::Indent,
            TokenKind::String("required".to_owned()),
            TokenKind::Colon,
            TokenKind::LeftBracket,
            TokenKind::String("summary".to_owned()),
            TokenKind::RightBracket,
            TokenKind::Comma,
            TokenKind::Newline,
            TokenKind::String("flag".to_owned()),
            TokenKind::Colon,
            kw(Keyword::True),
            TokenKind::Newline,
            TokenKind::Dedent,
        ]
    );
    Ok(())
}

#[test]
fn fork_line_lexes_with_in_as_identifier() -> TestResult {
    // `in` is not a rev-2 keyword; the fork grammar owns it at parse level.
    let kinds = token_kinds(&lex("fork lens in config.lenses sequential\n")?);
    assert_eq!(
        kinds,
        vec![
            kw(Keyword::Fork),
            ident("lens"),
            ident("in"),
            ident("config"),
            accessor("lenses"),
            kw(Keyword::Sequential),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn spawn_join_wait_timeout_line_shapes_lex() -> TestResult {
    let kinds = token_kinds(&lex(
        "spawn audit(change_id: change_id)\njoin -> verdicts\nwait signoff timeout 2d -> decision\n",
    )?);
    assert_eq!(
        kinds,
        vec![
            kw(Keyword::Spawn),
            ident("audit"),
            TokenKind::LeftParen,
            ident("change_id"),
            TokenKind::Colon,
            ident("change_id"),
            TokenKind::RightParen,
            TokenKind::Newline,
            kw(Keyword::Join),
            TokenKind::Arrow,
            ident("verdicts"),
            TokenKind::Newline,
            kw(Keyword::Wait),
            ident("signoff"),
            kw(Keyword::Timeout),
            TokenKind::Duration {
                magnitude: 2,
                unit: DurationUnit::Days,
            },
            TokenKind::Arrow,
            ident("decision"),
            TokenKind::Newline,
        ]
    );
    Ok(())
}
