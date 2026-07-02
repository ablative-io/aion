//! Token readers shared by the control-flow walker.
//!
//! These are small, total functions over the [`Token`] stream: they bound
//! parenthesised calls and braced blocks, read a primitive's literal or
//! reference argument, resolve a `run` node's activity name, and render a short
//! snippet for an `Opaque` node. None panics; out-of-range or unbalanced input
//! yields a conservative answer (the gleam compiler is the authority on
//! well-formedness, not these readers).

use super::scan::Token;

const ACTIVITY_WRAPPER_SUFFIX: &str = "_activity";

/// Finds the index of the first `{` in `[start, end)`, or `None`. Used to locate
/// a `case` body opener or a function-body opener after its signature.
pub(super) fn find_open_brace(tokens: &[Token], start: usize, end: usize) -> Option<usize> {
    (start..end.min(tokens.len())).find(|&index| matches!(tokens[index], Token::OpenBrace))
}

/// Given the index of an `{`, returns the index of its matching `}` within
/// `[open, end)`, honouring nesting. Returns `None` if unbalanced.
pub(super) fn match_brace(tokens: &[Token], open: usize, end: usize) -> Option<usize> {
    let mut depth = 0_i32;
    let upper = end.min(tokens.len());
    for (index, token) in tokens.iter().enumerate().take(upper).skip(open) {
        match token {
            Token::OpenBrace => depth += 1,
            Token::CloseBrace => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// Returns the index just past a call's argument list, given the call's leading
/// token at `call`. The call's `(` is the first paren at or after `call + 1`;
/// the result is the index after its matching `)`. When the call has no paren
/// (a bare reference), returns `call + 1`. Bounded by `end`.
pub(super) fn end_of_call(tokens: &[Token], call: usize, end: usize) -> usize {
    let upper = end.min(tokens.len());
    let mut index = call + 1;
    // Skip a trailing member access chain so `a.b(...)` and `a.b.c(...)` are
    // handled, though the walker only calls this on a primitive or call head.
    if index >= upper {
        return upper;
    }
    if !matches!(tokens[index], Token::OpenParen) {
        return index;
    }
    let mut depth = 0_i32;
    while index < upper {
        match tokens[index] {
            Token::OpenParen => depth += 1,
            Token::CloseParen => {
                depth -= 1;
                if depth == 0 {
                    return index + 1;
                }
            }
            _ => {}
        }
        index += 1;
    }
    upper
}

/// Reads the activity name from a `run(<wrappers>.<name>_activity(...))` call.
///
/// The first token after `run`'s open paren must be a qualified call whose
/// member ends with `_activity`; the activity name is that member with the
/// suffix stripped. Returns `None` for any other shape, so a `run` the extractor
/// cannot resolve is a loud error rather than a blank node.
pub(super) fn run_activity_name(args: &[Token]) -> Option<String> {
    let mut index = 0;
    if matches!(args.first(), Some(Token::OpenParen)) {
        index = 1;
    }
    if let Some(Token::Qualified { left: _, right }) = args.get(index) {
        if let Some(name) = right.strip_suffix(ACTIVITY_WRAPPER_SUFFIX) {
            if !name.is_empty() {
                return Some(name.to_owned());
            }
        }
    }
    None
}

/// Reads the first string literal appearing as a call argument, scanning to the
/// matching close paren of the call. Returns `None` if no literal precedes it.
pub(super) fn first_string_literal(args: &[Token]) -> Option<String> {
    let mut depth = 0_i32;
    for token in args {
        match token {
            Token::OpenParen => depth += 1,
            Token::CloseParen => {
                depth -= 1;
                if depth <= 0 {
                    return None;
                }
            }
            Token::StringLiteral(literal) if depth >= 1 => return Some(literal.clone()),
            _ => {}
        }
    }
    None
}

/// Reads every fan-out member activity name from a concurrency call's argument
/// list (`all` / `race` / `map`). A member is a `<wrappers>.<name>_activity`
/// reference — whether applied (`a_activity(x)` in `all([..])`) or passed as a
/// function value (`a_activity` in `map([items], a_activity)`). Names are
/// returned in source order, the suffix stripped. Scans only within the call's
/// own parentheses so a following statement's activity is not captured.
pub(super) fn fan_out_members(args: &[Token]) -> Vec<String> {
    let mut members = Vec::new();
    let mut depth = 0_i32;
    let mut started = false;
    for token in args {
        match token {
            Token::OpenParen => {
                depth += 1;
                started = true;
            }
            Token::CloseParen => {
                depth -= 1;
                if started && depth <= 0 {
                    break;
                }
            }
            Token::Qualified { right, .. } if depth >= 1 => {
                if let Some(name) = right.strip_suffix(ACTIVITY_WRAPPER_SUFFIX) {
                    if !name.is_empty() {
                        members.push(name.to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    members
}

/// Reads the first non-paren argument as a reference token (an identifier or
/// qualified reference), for primitives whose first argument is a value rather
/// than a literal (`receive`, `cancel_timer`).
pub(super) fn first_argument_reference(args: &[Token]) -> Option<String> {
    let mut depth = 0_i32;
    for token in args {
        match token {
            Token::OpenParen => depth += 1,
            Token::CloseParen => {
                depth -= 1;
                if depth <= 0 {
                    return None;
                }
            }
            Token::Ident(word) if depth >= 1 => return Some(word.clone()),
            Token::Qualified { left, right } if depth >= 1 => {
                return Some(format!("{left}.{right}"));
            }
            _ => {}
        }
    }
    None
}

/// Returns the last top-level bare-identifier argument in a call's argument
/// range `[start, end)` — the entry function passed to `workflow.define`. Only
/// identifiers at the call's own paren depth are considered, so a nested call's
/// arguments are never mistaken for the entry function. A qualified or other
/// non-bare argument at that depth clears the candidate: the entry function is
/// a bare reference, not a qualified expression.
pub(super) fn last_identifier_argument(
    tokens: &[Token],
    start: usize,
    end: usize,
) -> Option<String> {
    let upper = end.min(tokens.len());
    let mut depth = 0_i32;
    let mut last: Option<String> = None;
    for token in tokens.iter().take(upper).skip(start) {
        match token {
            Token::OpenParen => depth += 1,
            Token::CloseParen => depth -= 1,
            // A bare identifier directly inside the call's own parens (depth 1)
            // is a candidate entry function; the last one wins.
            Token::Ident(word) if depth == 1 => last = Some(word.clone()),
            Token::Qualified { .. } if depth == 1 => last = None,
            _ => {}
        }
    }
    last
}

/// Reads the name of a leading bare local-function call in the scrutinee
/// `[start, end)`: an [`Token::Ident`] immediately followed by `(`. Returns the
/// function name, or `None` when the scrutinee does not open with such a call.
pub(super) fn leading_local_call(tokens: &[Token], start: usize, end: usize) -> Option<String> {
    let upper = end.min(tokens.len());
    if start >= upper {
        return None;
    }
    if let Token::Ident(name) = &tokens[start] {
        if matches!(tokens.get(start + 1), Some(Token::OpenParen)) {
            return Some(name.clone());
        }
    }
    None
}

/// Renders a compact, single-line snippet of a token slice for an `Opaque`
/// node's correlation, so a consumer can locate the unmodellable shape.
pub(super) fn snippet(tokens: &[Token]) -> String {
    let mut out = String::new();
    for token in tokens {
        if !out.is_empty() {
            out.push(' ');
        }
        match token {
            Token::Ident(word) => out.push_str(word),
            Token::Qualified { left, right } => {
                out.push_str(left);
                out.push('.');
                out.push_str(right);
            }
            Token::OpenParen => out.push('('),
            Token::CloseParen => out.push(')'),
            Token::OpenBrace => out.push('{'),
            Token::CloseBrace => out.push('}'),
            Token::Arrow => out.push_str("->"),
            Token::Comma => out.push(','),
            Token::StringLiteral(literal) => {
                out.push('"');
                out.push_str(literal);
                out.push('"');
            }
            Token::Other(c) => out.push(*c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::scan::tokenise;

    #[test]
    fn match_brace_honours_nesting() {
        let tokens = tokenise("{ a { b } c }");
        let close = match_brace(&tokens, 0, tokens.len())
            .unwrap_or_else(|| unreachable!("the source is brace-balanced"));
        assert!(matches!(tokens[close], Token::CloseBrace));
        assert_eq!(close, tokens.len() - 1);
    }

    #[test]
    fn end_of_call_skips_balanced_args() {
        let tokens = tokenise("run(wrappers.a_activity(x)) next");
        let after = end_of_call(&tokens, 0, tokens.len());
        assert_eq!(tokens.get(after), Some(&Token::Ident("next".to_owned())));
    }

    #[test]
    fn run_activity_name_strips_suffix() {
        let tokens = tokenise("(wrappers.reserve_inventory_activity(input))");
        assert_eq!(
            run_activity_name(&tokens),
            Some("reserve_inventory".to_owned())
        );
    }

    #[test]
    fn last_identifier_argument_reads_the_define_entry() {
        let tokens = tokenise("(\"gate\", codecs.a(), codecs.b(), codecs.c(), execute)");
        assert_eq!(
            last_identifier_argument(&tokens, 0, tokens.len()),
            Some("execute".to_owned())
        );
    }

    #[test]
    fn last_identifier_argument_rejects_a_qualified_last_argument() {
        let tokens = tokenise("(\"gate\", codecs.a(), helpers.execute)");
        assert_eq!(last_identifier_argument(&tokens, 0, tokens.len()), None);
    }

    #[test]
    fn last_identifier_argument_ignores_nested_call_arguments() {
        // `input` sits at depth 2 inside `wrap(input)`; only `execute` is at
        // the call's own depth.
        let tokens = tokenise("(\"gate\", wrap(input), execute)");
        assert_eq!(
            last_identifier_argument(&tokens, 0, tokens.len()),
            Some("execute".to_owned())
        );
    }

    #[test]
    fn leading_local_call_detects_helper() {
        let tokens = tokenise("charge_payment(input, reservation)");
        assert_eq!(
            leading_local_call(&tokens, 0, tokens.len()),
            Some("charge_payment".to_owned())
        );
    }

    #[test]
    fn leading_local_call_rejects_qualified() {
        let tokens = tokenise("decode.run(raw_input, decode.string)");
        assert_eq!(leading_local_call(&tokens, 0, tokens.len()), None);
    }
}
