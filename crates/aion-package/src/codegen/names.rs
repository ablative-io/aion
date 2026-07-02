//! Deterministic Gleam identifier helpers.
//!
//! Generated names are pure functions of the authored type names, so
//! regeneration is stable: a boundary type `OrderInput` derives the codec
//! prefix and emitted-schema stem `order_input` through [`pascal_to_snake`].
//! Uniqueness of the derived names is checked by the interface front-end
//! (the Gleam compiler already guarantees type and constructor names are
//! unique per module).

/// Words the Gleam compiler reserves (including reserved-for-future words);
/// none may appear as a package name, activity name, or record label.
const GLEAM_RESERVED: &[&str] = &[
    "as",
    "assert",
    "auto",
    "case",
    "const",
    "delegate",
    "derive",
    "echo",
    "else",
    "fn",
    "if",
    "implement",
    "import",
    "let",
    "macro",
    "opaque",
    "panic",
    "pub",
    "test",
    "todo",
    "type",
    "use",
];

/// Whether `text` is a valid Gleam `snake_case` identifier
/// (`^[a-z][a-z0-9_]*$`). Reservedness is checked separately by
/// [`is_reserved_word`].
pub(crate) fn is_snake_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Whether `text` is a Gleam reserved word.
pub(crate) fn is_reserved_word(text: &str) -> bool {
    GLEAM_RESERVED.contains(&text)
}

/// Converts a `PascalCase` type or constructor name to its `snake_case` form
/// (`OrderInput` → `order_input`). Pure and deterministic; used for codec
/// function prefixes, emitted-schema stems, and canonical enum wire strings.
pub(crate) fn pascal_to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (index, ch) in name.char_indices() {
        if ch.is_ascii_uppercase() {
            if index != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{is_reserved_word, is_snake_identifier, pascal_to_snake};

    #[test]
    fn snake_identifiers_are_classified() {
        for valid in ["a", "repo_root", "round_backoff_ms", "a1", "x_2_y"] {
            assert!(is_snake_identifier(valid), "{valid} should be valid");
        }
        for invalid in ["", "Repo", "1a", "a-b", "_a", "a b", "naïve"] {
            assert!(!is_snake_identifier(invalid), "{invalid} should be invalid");
        }
    }

    #[test]
    fn reserved_words_are_recognised() {
        assert!(is_reserved_word("type"));
        assert!(is_reserved_word("use"));
        assert!(!is_reserved_word("kind"));
    }

    #[test]
    fn pascal_to_snake_is_deterministic_and_shape_true() {
        assert_eq!(pascal_to_snake("OrderInput"), "order_input");
        assert_eq!(pascal_to_snake("Shipment"), "shipment");
        assert_eq!(
            pascal_to_snake("GateInputWorkspace"),
            "gate_input_workspace"
        );
        assert_eq!(pascal_to_snake("ClosedOut"), "closed_out");
        assert_eq!(pascal_to_snake("Vm"), "vm");
        assert_eq!(pascal_to_snake("A1B"), "a1_b");
    }
}
