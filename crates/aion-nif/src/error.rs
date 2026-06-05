//! Typed errors for term conversion and NIF declaration.

use thiserror::Error;

/// Describes failures while converting between beamr terms and Rust values.
///
/// `TermError` replaces beamr's bare `badarg` with a typed, descriptive
/// failure so NIF authors and the engine can report which conversion failed and
/// why.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TermError {
    /// A term had a different runtime kind than the conversion expected.
    #[error("expected term kind {expected}, found {found}")]
    TypeMismatch {
        /// The expected beamr term kind.
        expected: &'static str,
        /// The observed beamr term kind.
        found: String,
    },

    /// Decoding a positional NIF argument failed.
    #[error("failed to decode argument {index}: {source}")]
    ArgumentDecode {
        /// Zero-based argument index.
        index: usize,
        /// Underlying term-conversion failure.
        #[source]
        source: Box<TermError>,
    },

    /// Resolving an atom name through the process atom table failed.
    #[error("failed to resolve atom {atom}: {reason}")]
    AtomResolution {
        /// Atom name or identifier that could not be resolved.
        atom: String,
        /// Human-readable resolution failure reason.
        reason: String,
    },

    /// Allocating a term on the process heap failed.
    #[error("failed to allocate {shape} term on the process heap")]
    HeapAllocation {
        /// Term shape being allocated.
        shape: &'static str,
    },
}

/// Describes invalid NIF declarations before engine registration.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NifDeclError {
    /// A NIF set contains the same module/function/arity more than once.
    #[error("duplicate NIF declaration for {module}:{function}/{arity}")]
    Duplicate {
        /// Gleam or Elixir module name.
        module: String,
        /// Function name.
        function: String,
        /// Function arity.
        arity: u8,
    },

    /// A NIF arity cannot be represented by the beamr native registration API.
    #[error("invalid arity for {module}:{function}/{arity}")]
    InvalidArity {
        /// Gleam or Elixir module name.
        module: String,
        /// Function name.
        function: String,
        /// Out-of-range arity value.
        arity: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::{NifDeclError, TermError};

    #[test]
    fn term_error_display_describes_each_variant() {
        let type_mismatch = TermError::TypeMismatch {
            expected: "integer",
            found: "binary".to_owned(),
        };
        assert_eq!(
            type_mismatch.to_string(),
            "expected term kind integer, found binary"
        );

        let argument_decode = TermError::ArgumentDecode {
            index: 2,
            source: Box::new(type_mismatch.clone()),
        };
        assert_eq!(
            argument_decode.to_string(),
            "failed to decode argument 2: expected term kind integer, found binary"
        );

        let atom_resolution = TermError::AtomResolution {
            atom: "ok".to_owned(),
            reason: "atom table is unavailable".to_owned(),
        };
        assert_eq!(
            atom_resolution.to_string(),
            "failed to resolve atom ok: atom table is unavailable"
        );

        let heap_allocation = TermError::HeapAllocation { shape: "tuple" };
        assert_eq!(
            heap_allocation.to_string(),
            "failed to allocate tuple term on the process heap"
        );
    }

    #[test]
    fn duplicate_nif_display_includes_conflicting_identity() {
        let error = NifDeclError::Duplicate {
            module: "example/module".to_owned(),
            function: "render".to_owned(),
            arity: 2,
        };
        let display = error.to_string();

        assert!(display.contains("example/module"));
        assert!(display.contains("render"));
        assert!(display.contains('2'));
    }
}
