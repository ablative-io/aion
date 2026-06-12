//! Explicit inflate budgets for `.aion` archive extraction.
//!
//! ZIP entries declare their own compressed/uncompressed sizes, so a hostile
//! archive can lie and inflate far past any upload ceiling (DEFLATE bombs
//! reach ~1000:1). Extraction therefore charges every inflated byte against
//! an [`ExtractionLimits`] budget the caller chooses explicitly — there is
//! deliberately no `Default`.

use std::io::Read;

use zip::result::ZipError;

use crate::PackageError;

/// Caller-chosen budget on the total inflated size of all archive entries.
///
/// Every package-loading entry point requires one; the caller decides whether
/// the input is trusted ([`ExtractionLimits::unbounded`]) or hostile network
/// bytes ([`ExtractionLimits::bounded`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtractionLimits {
    max_inflated_bytes: Option<u64>,
}

impl ExtractionLimits {
    /// Caps the running total of inflated bytes across all archive entries
    /// (manifest included) at `max_inflated_bytes`. Exceeding the budget
    /// fails extraction loudly with
    /// [`PackageError::InflatedSizeExceeded`] — entries are never truncated.
    #[must_use]
    pub const fn bounded(max_inflated_bytes: u64) -> Self {
        Self {
            max_inflated_bytes: Some(max_inflated_bytes),
        }
    }

    /// No inflate ceiling.
    ///
    /// ONLY for trusted operator-local files (engine startup packages, CLI
    /// and build tooling reading archives they just wrote, test fixtures) —
    /// never for network input. A hostile archive under any upload ceiling
    /// can inflate ~1000:1 and exhaust memory if extracted unbounded.
    #[must_use]
    pub const fn unbounded() -> Self {
        Self {
            max_inflated_bytes: None,
        }
    }

    /// Begins one extraction's running budget.
    pub(crate) const fn budget(self) -> ExtractionBudget {
        match self.max_inflated_bytes {
            Some(limit) => ExtractionBudget::Bounded {
                limit,
                remaining: limit,
            },
            None => ExtractionBudget::Unbounded,
        }
    }
}

/// Running inflate budget consumed entry by entry during one extraction.
#[derive(Debug)]
pub(crate) enum ExtractionBudget {
    /// Trusted-input extraction with no inflate ceiling.
    Unbounded,
    /// Bounded extraction tracking the bytes still admissible.
    Bounded {
        /// The caller-configured ceiling, reported on refusal.
        limit: u64,
        /// Budget left for the remaining entries.
        remaining: u64,
    },
}

impl ExtractionBudget {
    /// Reads one archive entry to completion, charging its inflated size
    /// against the remaining budget.
    ///
    /// The reader is wrapped in [`Read::take`] at one byte past the remaining
    /// budget, so a single entry can never buffer meaningfully past the
    /// budget before the refusal fires.
    pub(crate) fn read_entry<R>(&mut self, reader: &mut R) -> Result<Vec<u8>, PackageError>
    where
        R: Read,
    {
        let mut bytes = Vec::new();
        match self {
            Self::Unbounded => {
                reader
                    .read_to_end(&mut bytes)
                    .map_err(|source| PackageError::ArchiveRead(ZipError::Io(source)))?;
            }
            Self::Bounded { limit, remaining } => {
                // One sentinel byte past the budget distinguishes "exactly on
                // budget" from "would exceed it" without unbounded buffering.
                let probe = remaining.saturating_add(1);
                let mut taken = reader.take(probe);
                taken
                    .read_to_end(&mut bytes)
                    .map_err(|source| PackageError::ArchiveRead(ZipError::Io(source)))?;
                let inflated = probe - taken.limit();
                if inflated > *remaining {
                    return Err(PackageError::InflatedSizeExceeded { limit: *limit });
                }
                *remaining -= inflated;
            }
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::ExtractionLimits;
    use crate::PackageError;

    #[test]
    fn bounded_budget_charges_across_entries() -> Result<(), PackageError> {
        let mut budget = ExtractionLimits::bounded(10).budget();

        let first = budget.read_entry(&mut &[0_u8; 6][..])?;
        assert_eq!(first.len(), 6);

        let second = budget.read_entry(&mut &[0_u8; 4][..])?;
        assert_eq!(second.len(), 4);

        let result = budget.read_entry(&mut &[0_u8; 1][..]);
        assert!(matches!(
            result,
            Err(PackageError::InflatedSizeExceeded { limit: 10 })
        ));
        Ok(())
    }

    #[test]
    fn single_entry_past_budget_is_refused_reporting_the_limit() {
        let mut budget = ExtractionLimits::bounded(4).budget();

        let result = budget.read_entry(&mut &[0_u8; 5][..]);

        assert!(matches!(
            result,
            Err(PackageError::InflatedSizeExceeded { limit: 4 })
        ));
    }

    #[test]
    fn unbounded_budget_reads_everything() -> Result<(), PackageError> {
        let mut budget = ExtractionLimits::unbounded().budget();

        let bytes = budget.read_entry(&mut &[0_u8; 4096][..])?;

        assert_eq!(bytes.len(), 4096);
        Ok(())
    }
}
