//! Integration tests for AWL lexer operators and literal forms.

use std::error::Error;

use aion_awl::{Keyword, Token, TokenKind, lex};

fn token_kinds(tokens: &[Token]) -> Vec<TokenKind> {
    tokens.iter().map(|token| token.kind.clone()).collect()
}

#[test]
fn strings_numbers_keywords_and_operators_lex() -> Result<(), Box<dyn Error>> {
    let source = r#"when not ok and count >= 3 or ratio < 1.5
action run() -> Result
retry 1 backoff 10s..3d
queue "a\n\t"
node "\""
node "\\"
"#;
    let kinds = token_kinds(&lex(source)?);

    assert_eq!(
        kinds,
        vec![
            TokenKind::Keyword(Keyword::When),
            TokenKind::Keyword(Keyword::Not),
            TokenKind::Identifier("ok".to_owned()),
            TokenKind::Keyword(Keyword::And),
            TokenKind::Identifier("count".to_owned()),
            TokenKind::GreaterEqual,
            TokenKind::Integer(3),
            TokenKind::Keyword(Keyword::Or),
            TokenKind::Identifier("ratio".to_owned()),
            TokenKind::Less,
            TokenKind::Float(1.5),
            TokenKind::Newline,
            TokenKind::Keyword(Keyword::Action),
            TokenKind::Identifier("run".to_owned()),
            TokenKind::LeftParen,
            TokenKind::RightParen,
            TokenKind::Arrow,
            TokenKind::TypeIdentifier("Result".to_owned()),
            TokenKind::Newline,
            TokenKind::Keyword(Keyword::Retry),
            TokenKind::Integer(1),
            TokenKind::Keyword(Keyword::Backoff),
            TokenKind::Duration {
                magnitude: 10,
                unit: aion_awl::DurationUnit::Seconds,
            },
            TokenKind::DotDot,
            TokenKind::Duration {
                magnitude: 3,
                unit: aion_awl::DurationUnit::Days,
            },
            TokenKind::Newline,
            TokenKind::Keyword(Keyword::Queue),
            TokenKind::String("a\n\t".to_owned()),
            TokenKind::Newline,
            TokenKind::Keyword(Keyword::Node),
            TokenKind::String("\"".to_owned()),
            TokenKind::Newline,
            TokenKind::Keyword(Keyword::Node),
            TokenKind::String("\\".to_owned()),
            TokenKind::Newline,
        ]
    );
    Ok(())
}

#[test]
fn equality_and_brackets_lex() -> Result<(), Box<dyn Error>> {
    let kinds = token_kinds(&lex("when [true, false] != [] == flag + suffix\n")?);

    assert_eq!(
        kinds,
        vec![
            TokenKind::Keyword(Keyword::When),
            TokenKind::LeftBracket,
            TokenKind::Keyword(Keyword::True),
            TokenKind::Comma,
            TokenKind::Keyword(Keyword::False),
            TokenKind::RightBracket,
            TokenKind::BangEqual,
            TokenKind::LeftBracket,
            TokenKind::RightBracket,
            TokenKind::EqualEqual,
            TokenKind::Identifier("flag".to_owned()),
            TokenKind::Plus,
            TokenKind::Identifier("suffix".to_owned()),
            TokenKind::Newline,
        ]
    );
    Ok(())
}
