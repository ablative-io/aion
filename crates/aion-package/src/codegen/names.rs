//! Deterministic Gleam identifier derivation and collision tracking.
//!
//! Generated names are pure functions of the schema file stem and the
//! property path, so regeneration is stable. Every generated type and
//! constructor name is claimed in a [`NameRegistry`]; a second claim of the
//! same name is a loud [`CodegenError::NameCollision`] naming both origins.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::error::CodegenError;

/// Words the Gleam compiler reserves (including reserved-for-future words);
/// none may appear as a record label.
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

/// Whether `value` can derive a Gleam constructor name segment: ASCII
/// letters and digits separated by `_` or `-`, starting with a letter, with
/// no empty segments.
pub(crate) fn is_constructor_safe(value: &str) -> bool {
    let mut first_char = true;
    let mut previous_separator = true;
    for c in value.chars() {
        match c {
            'a'..='z' | 'A'..='Z' => {
                first_char = false;
                previous_separator = false;
            }
            '0'..='9' => {
                if first_char {
                    return false;
                }
                previous_separator = false;
            }
            '_' | '-' => {
                if previous_separator {
                    return false;
                }
                previous_separator = true;
            }
            _ => return false,
        }
    }
    !first_char && !previous_separator
}

/// `PascalCase` form of a snake/kebab segment: `closed_out` → `ClosedOut`,
/// `hello-world` → `HelloWorld`. Pure and deterministic; the input must
/// already satisfy [`is_snake_identifier`] or [`is_constructor_safe`].
pub(crate) fn pascal_case(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for part in text.split(['_', '-']) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            output.extend(first.to_uppercase());
            output.push_str(chars.as_str());
        }
    }
    output
}

/// Type name for a property path: the `PascalCase` concatenation of every
/// segment (`["gate_input", "workspace"]` → `GateInputWorkspace`).
pub(crate) fn type_name(segments: &[String]) -> String {
    segments
        .iter()
        .map(|segment| pascal_case(segment))
        .collect()
}

/// Function-name prefix for a property path: the segments joined with `_`
/// (`["gate_input", "workspace"]` → `gate_input_workspace`).
pub(crate) fn fn_prefix(segments: &[String]) -> String {
    segments.join("_")
}

/// Converts a generated `PascalCase` type name to the `snake_case` schema stem
/// it derives from (`OrderInput` → `order_input`). The inverse shape of
/// [`type_name`], used to name the `schemas/<stem>.json` document a type came
/// from — for the "schema missing" error hint and the golden test helpers.
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

/// Where a generated name was derived from, for collision reporting.
#[derive(Clone, Debug)]
pub(crate) struct NameOrigin {
    /// Schema file the name was derived from.
    pub(crate) file: PathBuf,
    /// JSON pointer of the deriving construct.
    pub(crate) pointer: String,
}

/// Tracks every generated type and constructor name across the module.
///
/// Gleam type names and constructor names live in separate namespaces, so
/// they are tracked separately; a record's constructor (which shares its
/// type's name) is claimed in both.
#[derive(Debug, Default)]
pub(crate) struct NameRegistry {
    types: HashMap<String, NameOrigin>,
    constructors: HashMap<String, NameOrigin>,
}

impl NameRegistry {
    /// Claims a generated type name, failing on a second claim.
    pub(crate) fn claim_type(
        &mut self,
        name: &str,
        file: &Path,
        pointer: &str,
    ) -> Result<(), CodegenError> {
        claim(&mut self.types, name, file, pointer)
    }

    /// Claims a generated constructor name, failing on a second claim.
    pub(crate) fn claim_constructor(
        &mut self,
        name: &str,
        file: &Path,
        pointer: &str,
    ) -> Result<(), CodegenError> {
        claim(&mut self.constructors, name, file, pointer)
    }
}

fn claim(
    names: &mut HashMap<String, NameOrigin>,
    name: &str,
    file: &Path,
    pointer: &str,
) -> Result<(), CodegenError> {
    if let Some(first) = names.get(name) {
        return Err(CodegenError::NameCollision {
            name: name.to_owned(),
            first_file: first.file.clone(),
            first_pointer: first.pointer.clone(),
            second_file: file.to_path_buf(),
            second_pointer: pointer.to_owned(),
        });
    }
    names.insert(
        name.to_owned(),
        NameOrigin {
            file: file.to_path_buf(),
            pointer: pointer.to_owned(),
        },
    );
    Ok(())
}

/// Appends one JSON-pointer token to `pointer`, escaping `~` and `/` per
/// RFC 6901.
pub(crate) fn pointer_join(pointer: &str, token: &str) -> String {
    let escaped = token.replace('~', "~0").replace('/', "~1");
    format!("{pointer}/{escaped}")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        NameRegistry, fn_prefix, is_constructor_safe, is_reserved_word, is_snake_identifier,
        pascal_case, pointer_join, type_name,
    };
    use crate::codegen::error::CodegenError;

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
    fn constructor_safety_is_classified() {
        for valid in ["local", "closed_out", "hello-world", "Vm", "h2"] {
            assert!(is_constructor_safe(valid), "{valid} should be safe");
        }
        for invalid in ["", "2fast", "_x", "x_", "a__b", "a b", "ok!"] {
            assert!(!is_constructor_safe(invalid), "{invalid} should be unsafe");
        }
    }

    #[test]
    fn pascal_case_joins_segments() {
        assert_eq!(pascal_case("closed_out"), "ClosedOut");
        assert_eq!(pascal_case("hello-world"), "HelloWorld");
        assert_eq!(pascal_case("vm"), "Vm");
        assert_eq!(pascal_case("a1_b"), "A1B");
    }

    #[test]
    fn path_names_are_deterministic() {
        let segments = vec!["gate_input".to_owned(), "workspace".to_owned()];

        assert_eq!(type_name(&segments), "GateInputWorkspace");
        assert_eq!(fn_prefix(&segments), "gate_input_workspace");
    }

    #[test]
    fn pointer_join_escapes_rfc6901_specials() {
        assert_eq!(pointer_join("", "properties"), "/properties");
        assert_eq!(pointer_join("/a", "b/c"), "/a/b~1c");
        assert_eq!(pointer_join("/a", "t~de"), "/a/t~0de");
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn second_type_claim_reports_both_origins() -> TestResult {
        let mut registry = NameRegistry::default();
        registry.claim_type("Input", Path::new("schemas/input.json"), "")?;

        let result = registry.claim_type("Input", Path::new("schemas/other.json"), "/properties/x");
        let Err(CodegenError::NameCollision {
            name,
            first_file,
            second_file,
            second_pointer,
            ..
        }) = result
        else {
            return Err("second claim must collide".into());
        };
        assert_eq!(name, "Input");
        assert_eq!(first_file, Path::new("schemas/input.json"));
        assert_eq!(second_file, Path::new("schemas/other.json"));
        assert_eq!(second_pointer, "/properties/x");
        Ok(())
    }

    #[test]
    fn constructor_namespace_is_separate_from_types() -> TestResult {
        let mut registry = NameRegistry::default();
        registry.claim_type("Input", Path::new("a.json"), "")?;

        assert!(
            registry
                .claim_constructor("Input", Path::new("a.json"), "")
                .is_ok(),
            "constructors and types are separate Gleam namespaces"
        );
        assert!(
            registry
                .claim_constructor("Input", Path::new("b.json"), "")
                .is_err()
        );
        Ok(())
    }
}
