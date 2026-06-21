//! Splitting a `case` body into its arms and labelling each arm.
//!
//! A Gleam `case` body is a run of `<pattern> -> <body>` arms. The body is
//! either a brace-delimited block (`-> { ... }`) or a bare expression
//! (`-> charge_payment(input, reservation)`). Arm boundaries are recovered from
//! the depth-0 `->` separators plus the brace structure of block bodies; an
//! expression body is bounded by the start of the next arm's pattern, found by a
//! short backward scan over the next pattern's balanced shape.
//!
//! This is not a full pattern parser. It recovers exactly the boundaries the
//! control-flow walker needs to recurse each arm faithfully, and labels the arm
//! by its leading constructor (`Ok` / `Error` / a named constructor / `_`).

use super::model::ArmLabel;
use super::reader::match_brace;
use super::scan::Token;

/// One `case` arm: the start of its pattern (for labelling) and the half-open
/// range of its body (for recursion).
#[derive(Clone, Copy)]
pub(super) struct Arm {
    /// Inclusive start of the arm's pattern — its leading constructor token.
    pub(super) pattern_start: usize,
    /// Inclusive start of the arm's body (just past the `->`, or just inside the
    /// block's `{`).
    pub(super) body_start: usize,
    /// Exclusive end of the arm's body.
    pub(super) body_end: usize,
}

/// Splits the `case` body `[start, end)` into its arms.
pub(super) fn split_arms(tokens: &[Token], start: usize, end: usize) -> Vec<Arm> {
    let mut arms = Vec::new();
    let mut cursor = start;
    while cursor < end {
        let Some(arrow) = next_top_level_arrow(tokens, cursor, end) else {
            break;
        };
        let pattern_start = cursor;
        let body_start = arrow + 1;
        let (body_start, body_end, next) = bound_body(tokens, body_start, end);
        arms.push(Arm {
            pattern_start,
            body_start,
            body_end,
        });
        if next <= cursor {
            break;
        }
        cursor = next;
    }
    arms
}

/// Bounds an arm body that begins at `body_start`, returning the body's
/// `(start, end)` and the cursor for the next arm.
///
/// A block body (`{ ... }`) is delimited by braces; the inner range excludes the
/// braces. An expression body runs to the start of the next arm's pattern, found
/// by locating the following depth-0 `->` and scanning its pattern back.
fn bound_body(tokens: &[Token], body_start: usize, end: usize) -> (usize, usize, usize) {
    if matches!(tokens.get(body_start), Some(Token::OpenBrace)) {
        if let Some(close) = match_brace(tokens, body_start, end) {
            return (body_start + 1, close, close + 1);
        }
        // Unbalanced block: consume to the end of the case body.
        return (body_start + 1, end, end);
    }
    match next_top_level_arrow(tokens, body_start, end) {
        Some(next_arrow) => {
            let next_pattern = pattern_start_before(tokens, body_start, next_arrow);
            (body_start, next_pattern, next_pattern)
        }
        None => (body_start, end, end),
    }
}

/// Finds the next `->` in `[from, end)` at the case body's top level (paren and
/// brace depth zero).
fn next_top_level_arrow(tokens: &[Token], from: usize, end: usize) -> Option<usize> {
    let mut depth = 0_i32;
    let upper = end.min(tokens.len());
    for (index, token) in tokens.iter().enumerate().take(upper).skip(from) {
        match token {
            Token::OpenParen | Token::OpenBrace => depth += 1,
            Token::CloseParen | Token::CloseBrace => depth -= 1,
            Token::Arrow if depth == 0 => return Some(index),
            _ => {}
        }
    }
    None
}

/// Given an expression body in `[body_start, next_arrow)`, returns the index
/// where the next arm's pattern begins. The next pattern is the minimal suffix
/// before `next_arrow` that forms a constructor pattern: an optional balanced
/// `(...)` argument group preceded by one constructor token (an identifier, `_`,
/// or a literal). The body ends where that pattern begins.
fn pattern_start_before(tokens: &[Token], body_start: usize, next_arrow: usize) -> usize {
    if next_arrow == 0 {
        return body_start;
    }
    let mut index = next_arrow; // exclusive upper bound
    // Skip a trailing balanced `(...)` argument group, if present.
    if index > body_start && matches!(tokens.get(index - 1), Some(Token::CloseParen)) {
        let mut depth = 0_i32;
        let mut scan = index - 1;
        loop {
            match tokens.get(scan) {
                Some(Token::CloseParen) => depth += 1,
                Some(Token::OpenParen) => {
                    depth -= 1;
                    if depth == 0 {
                        index = scan;
                        break;
                    }
                }
                _ => {}
            }
            if scan == body_start {
                break;
            }
            scan -= 1;
        }
    }
    // The constructor token precedes the (optional) argument group.
    if index > body_start {
        index -= 1;
    }
    index.max(body_start)
}

/// Labels an arm by the leading constructor of its pattern.
pub(super) fn arm_label(tokens: &[Token], arm: &Arm) -> ArmLabel {
    match tokens.get(arm.pattern_start) {
        Some(Token::Ident(word)) if word == "Ok" => ArmLabel::Ok,
        Some(Token::Ident(word)) if word == "Error" => ArmLabel::Error,
        Some(Token::Ident(word)) if word == "_" => ArmLabel::Wildcard,
        Some(Token::Ident(word)) => ArmLabel::Pattern(word.clone()),
        Some(Token::Qualified { left, right }) => ArmLabel::Pattern(format!("{left}.{right}")),
        _ => ArmLabel::Wildcard,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::scan::tokenise;

    fn arms_of(body: &str) -> (Vec<Token>, Vec<Arm>) {
        // Wrap in a `case x { ... }` and split the inner body.
        let src = format!("case x {{ {body} }}");
        let tokens = tokenise(&src);
        // The inner body lies between the first `{` and the last `}`.
        let open = tokens
            .iter()
            .position(|t| matches!(t, Token::OpenBrace))
            .unwrap_or_else(|| unreachable!("wrapped source has an opening brace"));
        let close = tokens
            .iter()
            .rposition(|t| matches!(t, Token::CloseBrace))
            .unwrap_or_else(|| unreachable!("wrapped source has a closing brace"));
        let arms = split_arms(&tokens, open + 1, close);
        (tokens, arms)
    }

    #[test]
    fn two_expression_arms_split_cleanly() {
        let (tokens, arms) = arms_of("Ok(a) -> charge(a) Error(e) -> done(e)");
        assert_eq!(arms.len(), 2);
        assert_eq!(arm_label(&tokens, &arms[0]), ArmLabel::Ok);
        assert_eq!(arm_label(&tokens, &arms[1]), ArmLabel::Error);
        // Arm 0's body must not include the `Error` pattern of arm 1.
        let body0: Vec<_> = tokens[arms[0].body_start..arms[0].body_end].to_vec();
        assert!(!body0.contains(&Token::Ident("Error".to_owned())));
        assert!(body0.contains(&Token::Ident("charge".to_owned())));
    }

    #[test]
    fn block_body_arm_is_bounded_by_braces() {
        let (tokens, arms) = arms_of("Ok(a) -> ok(a) Error(e) -> { comp(e) more(e) }");
        assert_eq!(arms.len(), 2);
        let body1: Vec<_> = tokens[arms[1].body_start..arms[1].body_end].to_vec();
        assert!(body1.contains(&Token::Ident("comp".to_owned())));
        assert!(body1.contains(&Token::Ident("more".to_owned())));
    }

    #[test]
    fn wildcard_arm_is_labelled() {
        let (tokens, arms) = arms_of("_ -> fallback()");
        assert_eq!(arms.len(), 1);
        assert_eq!(arm_label(&tokens, &arms[0]), ArmLabel::Wildcard);
    }
}
