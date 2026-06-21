//! A minimal, total scanner over Gleam source.
//!
//! The extractor recognises a small, fixed vocabulary (`aion/workflow`'s
//! primitives) by their call syntax. To do that soundly it must not be fooled
//! by the same words appearing inside string literals or comments, and it must
//! read the literal arguments some primitives carry (a timer name, a child
//! name). This scanner produces exactly the events the extractor needs:
//! qualified-call sites, bare identifiers, and the structural punctuation
//! (`(` `)` `{` `}` `->` `,`) that bounds function bodies and `case` arms —
//! with comments and string contents excluded from matching but string
//! *literals* recoverable as call arguments. `case` is not a keyword token; it
//! arrives as `Token::Ident("case")`, which the control-flow walker recognises.
//!
//! It is deliberately not a full Gleam parser. Its job is to make source-text
//! extraction over a known, small surface sound — not to understand arbitrary
//! Gleam. Anything outside the recognised vocabulary is simply not emitted.

/// One lexical item the extractor cares about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Token {
    /// A bare identifier (snake or otherwise) not followed by a `.`.
    Ident(String),
    /// A qualified reference `left.right` (for example `workflow.run`).
    Qualified {
        /// The qualifier (module alias or value) left of the dot.
        left: String,
        /// The member right of the dot.
        right: String,
    },
    /// An opening parenthesis `(`.
    OpenParen,
    /// A closing parenthesis `)`.
    CloseParen,
    /// An opening brace `{` — a block or `case` body opener.
    OpenBrace,
    /// A closing brace `}` — a block or `case` body closer.
    CloseBrace,
    /// The case-arm arrow `->`.
    Arrow,
    /// A comma `,` — an argument or pattern separator.
    Comma,
    /// A string literal, with its already-unescaped contents.
    StringLiteral(String),
    /// Any other single punctuation/operator character the extractor ignores
    /// but must still see to reason about argument boundaries.
    Other(char),
}

/// Tokenises `source` into the items the extractor consumes.
///
/// Line comments (`//` to end of line, which also covers Gleam's `///` and
/// `////` doc comments) and the contents of string literals are excluded from
/// identifier and keyword matching. A string literal is preserved as a
/// [`Token::StringLiteral`] so the extractor can read a primitive's literal
/// argument. The scanner never panics and never allocates unboundedly beyond
/// the source length.
pub(crate) fn tokenise(source: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = source.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        if current == '/' && chars.get(index + 1) == Some(&'/') {
            index = skip_line_comment(&chars, index);
        } else if current.is_whitespace() {
            // Whitespace is not significant to the recognised vocabulary; drop
            // it so callers see adjacent tokens (`import` then `aion`) without
            // interleaved spacing.
            index += 1;
        } else if current == '"' {
            let (literal, next) = read_string(&chars, index);
            tokens.push(Token::StringLiteral(literal));
            index = next;
        } else if current == '(' {
            tokens.push(Token::OpenParen);
            index += 1;
        } else if current == ')' {
            tokens.push(Token::CloseParen);
            index += 1;
        } else if current == '{' {
            tokens.push(Token::OpenBrace);
            index += 1;
        } else if current == '}' {
            tokens.push(Token::CloseBrace);
            index += 1;
        } else if current == ',' {
            tokens.push(Token::Comma);
            index += 1;
        } else if current == '-' && chars.get(index + 1) == Some(&'>') {
            tokens.push(Token::Arrow);
            index += 2;
        } else if is_ident_start(current) {
            let (ident, next) = read_ident(&chars, index);
            index = next;
            if chars.get(index) == Some(&'.')
                && chars.get(index + 1).is_some_and(|c| is_ident_start(*c))
            {
                let (member, after) = read_ident(&chars, index + 1);
                tokens.push(Token::Qualified {
                    left: ident,
                    right: member,
                });
                index = after;
            } else {
                tokens.push(Token::Ident(ident));
            }
        } else {
            tokens.push(Token::Other(current));
            index += 1;
        }
    }
    tokens
}

fn skip_line_comment(chars: &[char], start: usize) -> usize {
    let mut index = start;
    while index < chars.len() && chars[index] != '\n' {
        index += 1;
    }
    index
}

/// Reads a double-quoted string starting at `start` (the opening quote),
/// returning the unescaped contents and the index just past the closing quote.
/// An unterminated string consumes to end of input, which the extractor treats
/// as a literal with whatever it captured — the gleam compiler is the authority
/// on well-formedness, not this scanner.
fn read_string(chars: &[char], start: usize) -> (String, usize) {
    let mut literal = String::new();
    let mut index = start + 1;
    while index < chars.len() {
        let current = chars[index];
        if current == '\\' {
            if let Some(next) = chars.get(index + 1) {
                literal.push(unescape(*next));
                index += 2;
                continue;
            }
            index += 1;
        } else if current == '"' {
            return (literal, index + 1);
        } else {
            literal.push(current);
            index += 1;
        }
    }
    (literal, index)
}

fn unescape(escaped: char) -> char {
    match escaped {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        other => other,
    }
}

fn read_ident(chars: &[char], start: usize) -> (String, usize) {
    let mut ident = String::new();
    let mut index = start;
    while index < chars.len() && is_ident_continue(chars[index]) {
        ident.push(chars[index]);
        index += 1;
    }
    (ident, index)
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::{Token, tokenise};

    #[test]
    fn qualified_calls_are_recognised() {
        let tokens = tokenise("workflow.run(x)");
        assert_eq!(
            tokens.first(),
            Some(&Token::Qualified {
                left: "workflow".to_owned(),
                right: "run".to_owned(),
            })
        );
    }

    #[test]
    fn line_comments_are_excluded() {
        let tokens = tokenise("// workflow.run(should_not_match)\nworkflow.all(y)");
        let qualified: Vec<_> = tokens
            .iter()
            .filter_map(|token| match token {
                Token::Qualified { left, right } => Some((left.as_str(), right.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(qualified, vec![("workflow", "all")]);
    }

    #[test]
    fn string_contents_do_not_match_but_are_recoverable() {
        let tokens = tokenise("start_timer(\"workflow.run\")");
        assert!(tokens.contains(&Token::StringLiteral("workflow.run".to_owned())));
        assert!(!tokens.iter().any(|token| matches!(
            token,
            Token::Qualified { left, right } if left == "workflow" && right == "run"
        )));
    }

    #[test]
    fn escapes_in_strings_are_unescaped() {
        let tokens = tokenise("(\"a\\nb\")");
        assert!(tokens.contains(&Token::StringLiteral("a\nb".to_owned())));
    }

    #[test]
    fn braces_arrow_and_comma_are_emitted() {
        let tokens = tokenise("case x { Ok(a) -> b, Error(e) -> c }");
        assert!(tokens.contains(&Token::OpenBrace));
        assert!(tokens.contains(&Token::CloseBrace));
        assert!(tokens.contains(&Token::Arrow));
        assert!(tokens.contains(&Token::Comma));
        // `case` is a bare identifier, not a keyword token.
        assert!(tokens.contains(&Token::Ident("case".to_owned())));
    }

    #[test]
    fn arrow_does_not_swallow_a_lone_minus() {
        // A standalone `-` (subtraction) is not an arrow.
        let tokens = tokenise("a - b");
        assert!(tokens.contains(&Token::Other('-')));
        assert!(!tokens.contains(&Token::Arrow));
    }
}
