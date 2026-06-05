//! Typed errors for term conversion and NIF declaration.

use std::{error::Error, fmt};

/// Describes failures while converting between beamr terms and Rust values.
///
/// `TermError` replaces beamr's bare `badarg` with a typed, descriptive
/// failure so NIF authors and the engine can report which conversion failed and
/// why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermError {
    /// A term had a different runtime kind than the conversion expected.
    TypeMismatch {
        /// The expected beamr term kind.
        expected: &'static str,
        /// The observed beamr term kind.
        found: String,
    },

    /// Decoding a positional NIF argument failed.
    ArgumentDecode {
        /// Zero-based argument index.
        index: usize,
        /// Underlying term-conversion failure.
        source: Box<TermError>,
    },

    /// Resolving an atom name through the process atom table failed.
    AtomResolution {
        /// Atom name or identifier that could not be resolved.
        atom: String,
        /// Human-readable resolution failure reason.
        reason: String,
    },

    /// Allocating a term on the process heap failed.
    HeapAllocation {
        /// Term shape being allocated.
        shape: &'static str,
    },

    /// A structured conversion through JSON or payload encoding failed.
    Conversion {
        /// Conversion boundary that failed.
        context: &'static str,
        /// Human-readable conversion failure reason.
        message: String,
    },
}

impl fmt::Display for TermError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TypeMismatch { expected, found } => {
                write!(formatter, "expected term kind {expected}, found {found}")
            }
            Self::ArgumentDecode { index, source } => {
                write!(formatter, "failed to decode argument {index}: {source}")
            }
            Self::AtomResolution { atom, reason } => {
                write!(formatter, "failed to resolve atom {atom}: {reason}")
            }
            Self::HeapAllocation { shape } => {
                write!(
                    formatter,
                    "failed to allocate {shape} term on the process heap"
                )
            }
            Self::Conversion { context, message } => {
                write!(formatter, "{context} conversion failed: {message}")
            }
        }
    }
}

impl Error for TermError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ArgumentDecode { source, .. } => Some(source.as_ref()),
            Self::TypeMismatch { .. }
            | Self::AtomResolution { .. }
            | Self::HeapAllocation { .. }
            | Self::Conversion { .. } => None,
        }
    }
}

/// Describes invalid NIF declarations before engine registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NifDeclError {
    /// A NIF set contains the same module/function/arity more than once.
    Duplicate {
        /// Gleam or Elixir module name.
        module: String,
        /// Function name.
        function: String,
        /// Function arity.
        arity: u8,
    },

    /// A NIF arity cannot be represented by the beamr native registration API.
    InvalidArity {
        /// Gleam or Elixir module name.
        module: String,
        /// Function name.
        function: String,
        /// Out-of-range arity value.
        arity: usize,
    },
}

impl fmt::Display for NifDeclError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate {
                module,
                function,
                arity,
            } => write!(
                formatter,
                "duplicate NIF declaration for {module}:{function}/{arity}"
            ),
            Self::InvalidArity {
                module,
                function,
                arity,
            } => write!(formatter, "invalid arity for {module}:{function}/{arity}"),
        }
    }
}

impl Error for NifDeclError {}

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

        let conversion = TermError::Conversion {
            context: "payload",
            message: "invalid json".to_owned(),
        };
        assert_eq!(
            conversion.to_string(),
            "payload conversion failed: invalid json"
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
