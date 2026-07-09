use crate::ast::{CallExpr, CallTarget};

use super::ParseError;
use super::expressions::parse_expr_at;
use super::source::{SourceLine, keyword_rest};

pub(super) fn parse_do(line: &SourceLine) -> Result<CallTarget, ParseError> {
    let rest = keyword_rest(line, "do", "do field needs a call target")?;
    if let Some(child) = rest.strip_prefix("child ") {
        let call = parse_call_text(line, child)?;
        Ok(CallTarget::Child {
            span: call.span,
            workflow: call.name,
            args: call.args,
        })
    } else {
        Ok(CallTarget::Action(parse_call_text(line, rest)?))
    }
}

fn parse_call_text(line: &SourceLine, text: &str) -> Result<CallExpr, ParseError> {
    let base = line.fragment_span(text);
    let open = text
        .find('(')
        .ok_or_else(|| ParseError::new(base, "do target must be a call"))?;
    let close = text
        .rfind(')')
        .ok_or_else(|| ParseError::new(base, "do target call needs closing `)`"))?;
    if text[close + 1..].trim().is_empty() {
        let mut args = Vec::new();
        for part in comma_parts(&text[open + 1..close]) {
            if part.trim().is_empty() {
                continue;
            }
            args.push(parse_expr_at(line, part.trim())?);
        }
        Ok(CallExpr {
            span: base,
            name: text[..open].trim().to_owned(),
            args,
        })
    } else {
        Err(ParseError::new(base, "unexpected text after call"))
    }
}

pub(super) fn comma_parts(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, ch) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else {
            match ch {
                '"' => in_string = true,
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(&text[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
    }
    parts.push(&text[start..]);
    parts
}
