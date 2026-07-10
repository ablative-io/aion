//! Identifier discipline for generated Gleam: case conversion, reserved-word
//! sanitization (panel-hardened in the AWL-0 emitter and carried over), and
//! collision-free synthesis of generated type/constructor names.

use std::collections::BTreeSet;

/// Reserved words in Gleam that cannot be used as value identifiers.
pub(super) fn is_gleam_keyword(name: &str) -> bool {
    matches!(
        name,
        "as" | "assert"
            | "auto"
            | "case"
            | "const"
            | "delegate"
            | "derive"
            | "echo"
            | "else"
            | "fn"
            | "if"
            | "implement"
            | "import"
            | "let"
            | "macro"
            | "opaque"
            | "panic"
            | "pub"
            | "test"
            | "todo"
            | "type"
            | "use"
    )
}

/// Sanitize an AWL identifier for emission: Gleam reserved words gain a
/// trailing underscore, applied consistently at every emission site. Wire
/// JSON keys always keep the AWL spelling.
pub(super) fn ident(name: &str) -> String {
    if is_gleam_keyword(name) {
        format!("{name}_")
    } else {
        name.to_owned()
    }
}

pub(super) fn pascal(name: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for character in name.chars() {
        if character == '_' {
            upper = true;
        } else if upper {
            out.extend(character.to_uppercase());
            upper = false;
        } else {
            out.push(character);
        }
    }
    out
}

pub(super) fn snake(name: &str) -> String {
    let mut out = String::new();
    for (index, character) in name.chars().enumerate() {
        if character.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(character.to_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

pub(super) fn string_lit(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Registry of `TitleCase` names already claimed by generated Gleam types and
/// value constructors; both share the claim set because a Gleam custom-type
/// constructor clashing with another constructor is a compile error.
#[derive(Debug, Default)]
pub(super) struct UpperNames {
    taken: BTreeSet<String>,
}

impl UpperNames {
    /// Claim `candidate` verbatim, returning whether it was free.
    pub(super) fn claim(&mut self, candidate: &str) -> bool {
        self.taken.insert(candidate.to_owned())
    }

    /// Claim `base`, or the first `base2`, `base3`, … suffix that is free.
    pub(super) fn fresh(&mut self, base: &str) -> String {
        if self.claim(base) {
            return base.to_owned();
        }
        let mut counter = 2_u32;
        loop {
            let candidate = format!("{base}{counter}");
            if self.claim(&candidate) {
                return candidate;
            }
            counter = counter.saturating_add(1);
        }
    }
}
