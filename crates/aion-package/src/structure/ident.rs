//! Gleam identifier rules used by structure extraction and regeneration.
//!
//! The codegen module carries equivalent `pub(crate)` validators, but they live
//! behind a private `mod names;` and are not reachable from this sibling
//! module. Rather than widen codegen's surface, the small, self-contained rules
//! the structure layer needs are restated here. They are pure and total.

/// Gleam reserved words (including reserved-for-future words); none may name a
/// generated activity, module, or wrapper.
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

/// Whether `text` is a valid Gleam `snake_case` identifier (`^[a-z][a-z0-9_]*$`).
/// Reservedness is checked separately by [`is_reserved_word`].
#[must_use]
pub(crate) fn is_snake_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Whether `text` is a Gleam reserved word.
#[must_use]
pub(crate) fn is_reserved_word(text: &str) -> bool {
    GLEAM_RESERVED.contains(&text)
}

#[cfg(test)]
mod tests {
    use super::{is_reserved_word, is_snake_identifier};

    #[test]
    fn snake_identifiers_are_classified() {
        for valid in ["a", "reserve_inventory", "charge_payment", "a1", "x_2_y"] {
            assert!(is_snake_identifier(valid), "{valid} should be valid");
        }
        for invalid in ["", "Reserve", "1a", "a-b", "_a", "a b", "naïve"] {
            assert!(!is_snake_identifier(invalid), "{invalid} should be invalid");
        }
    }

    #[test]
    fn reserved_words_are_recognised() {
        assert!(is_reserved_word("type"));
        assert!(is_reserved_word("case"));
        assert!(!is_reserved_word("reserve_inventory"));
    }
}
