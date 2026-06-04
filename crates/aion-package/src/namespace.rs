//! Pure logical-name <-> deployed-name bijection for content-hash namespacing.

use std::collections::BTreeSet;
use std::str::FromStr;

use crate::{BeamSet, ContentHash, hash::ContentHashParseError};

/// Separator between a logical module name and its package content hash.
///
/// The `.aion` format mandates the literal `$` character so engine code can
/// split deployed module names on the identical boundary. Gleam logical module
/// names do not contain `$`, keeping valid workflow module names unambiguous.
pub const DEPLOYED_NAME_SEPARATOR: char = '$';

/// A deployed module name parsed into its logical module name and content hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedDeployedName {
    logical: String,
    hash: ContentHash,
}

impl ParsedDeployedName {
    /// Creates a parsed deployed-name value from its two components.
    #[must_use]
    pub fn new(logical: String, hash: ContentHash) -> Self {
        Self { logical, hash }
    }

    /// Returns the logical module name before deployment-time namespacing.
    #[must_use]
    pub fn logical(&self) -> &str {
        &self.logical
    }

    /// Returns the content hash embedded in the deployed module name.
    #[must_use]
    pub const fn hash(&self) -> &ContentHash {
        &self.hash
    }

    /// Consumes the parsed value into its owned components.
    #[must_use]
    pub fn into_parts(self) -> (String, ContentHash) {
        (self.logical, self.hash)
    }
}

/// Errors produced when parsing a deployed module name.
#[derive(thiserror::Error, Clone, Debug, PartialEq, Eq)]
pub enum NamespaceError {
    /// The deployed module name did not contain the mandated separator.
    #[error("deployed module name is missing the '$' namespace separator")]
    MissingSeparator,

    /// The logical module-name component was empty.
    #[error("deployed module name has an empty logical module component")]
    EmptyLogicalName,

    /// The logical module-name component contained the mandated separator.
    #[error(
        "deployed module name has a logical module component containing the '$' namespace separator"
    )]
    SeparatorInLogicalName,

    /// The hash component was not a valid content-hash textual form.
    #[error("deployed module name has an invalid content hash: {source}")]
    InvalidHash {
        /// Hash parser error from the hash component.
        source: ContentHashParseError,
    },
}

/// Returns the deployed module name for a logical module in a package version.
///
/// This is the application-level versioning layer on top of beamr's VM-level
/// dual-version hot-loading: the engine registers each workflow module under a
/// logical-name plus content-hash name, allowing multiple immutable workflow
/// versions to coexist without reusing the bare logical module atom.
#[must_use]
pub fn deployed_name(logical: &str, hash: &ContentHash) -> String {
    format!("{logical}{DEPLOYED_NAME_SEPARATOR}{hash}")
}

/// Parses a deployed module name back into its logical module name and hash.
///
/// The parser accepts exactly one [`DEPLOYED_NAME_SEPARATOR`] boundary. Valid
/// Gleam logical module names do not contain the separator, so any additional
/// separator before the hash component is rejected as malformed rather than being
/// silently folded into the logical name.
///
/// # Errors
///
/// Returns [`NamespaceError::MissingSeparator`] when the separator is absent,
/// [`NamespaceError::EmptyLogicalName`] when the logical component is empty,
/// [`NamespaceError::SeparatorInLogicalName`] when the logical component contains
/// another separator, and [`NamespaceError::InvalidHash`] when the trailing
/// component is not a valid [`ContentHash`] textual form.
pub fn parse_deployed_name(deployed: &str) -> Result<ParsedDeployedName, NamespaceError> {
    let Some((logical, hash_text)) = deployed.split_once(DEPLOYED_NAME_SEPARATOR) else {
        return Err(NamespaceError::MissingSeparator);
    };

    if logical.is_empty() {
        return Err(NamespaceError::EmptyLogicalName);
    }
    if hash_text.contains(DEPLOYED_NAME_SEPARATOR) {
        return Err(NamespaceError::SeparatorInLogicalName);
    }

    let hash = ContentHash::from_str(hash_text)
        .map_err(|source| NamespaceError::InvalidHash { source })?;

    Ok(ParsedDeployedName::new(logical.to_owned(), hash))
}

/// Returns all deployed module names for a canonical beam set and package hash.
///
/// The returned set is the engine-ready registry name set for the package. The
/// content-hash suffix is what sidesteps beamr's two-deep same-name version limit
/// for workflow modules: different hashes produce disjoint deployed names even
/// when the logical module names are identical.
#[must_use]
pub fn deployed_names(beams: &BeamSet, hash: &ContentHash) -> BTreeSet<String> {
    beams
        .iter()
        .map(|module| deployed_name(module.name(), hash))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        DEPLOYED_NAME_SEPARATOR, NamespaceError, deployed_name, deployed_names, parse_deployed_name,
    };
    use crate::{BeamModule, BeamSet, ContentHash, hash::ContentHashParseError};

    fn hash(byte: u8) -> ContentHash {
        ContentHash::from_bytes([byte; 32])
    }

    fn beam_set() -> Result<BeamSet, crate::PackageError> {
        BeamSet::new(vec![
            BeamModule::new("workflow/b", vec![2]),
            BeamModule::new("workflow/a", vec![1]),
            BeamModule::new("stdlib/list", vec![3]),
        ])
    }

    #[test]
    fn forward_transform_uses_mandated_separator_and_hash_text() {
        let hash = hash(0xab);
        let deployed = deployed_name("order_workflow", &hash);

        assert_eq!(
            deployed,
            "order_workflow$abababababababababababababababababababababababababababababababab"
        );
        assert!(deployed.contains(DEPLOYED_NAME_SEPARATOR));
    }

    #[test]
    fn forward_then_inverse_round_trips_many_pairs() -> Result<(), NamespaceError> {
        let cases = [
            ("order_workflow", hash(0x00)),
            ("workflow_with_underscores", hash(0x11)),
            ("nested/module/name", hash(0x7f)),
            ("workflow_123", hash(0xff)),
        ];

        for (logical, hash) in cases {
            let parsed = parse_deployed_name(&deployed_name(logical, &hash))?;
            assert_eq!(parsed.logical(), logical);
            assert_eq!(parsed.hash(), &hash);
        }

        Ok(())
    }

    #[test]
    fn inverse_then_forward_recovers_deployed_name() -> Result<(), NamespaceError> {
        let original = deployed_name("workflow_with_underscores", &hash(0x42));
        let parsed = parse_deployed_name(&original)?;
        let recovered = deployed_name(parsed.logical(), parsed.hash());

        assert_eq!(recovered, original);
        Ok(())
    }

    #[test]
    fn parse_preserves_separator_neighbouring_chars() -> Result<(), NamespaceError> {
        let original = deployed_name("logical_name_with_underscores", &hash(0x33));
        let parsed = parse_deployed_name(&original)?;

        assert_eq!(parsed.logical(), "logical_name_with_underscores");
        assert_eq!(deployed_name(parsed.logical(), parsed.hash()), original);
        Ok(())
    }

    #[test]
    fn malformed_deployed_names_return_typed_errors() {
        assert_eq!(
            parse_deployed_name("workflow_without_hash"),
            Err(NamespaceError::MissingSeparator)
        );
        assert_eq!(
            parse_deployed_name(
                "$0000000000000000000000000000000000000000000000000000000000000000"
            ),
            Err(NamespaceError::EmptyLogicalName)
        );
        assert_eq!(
            parse_deployed_name("workflow$not-a-hash"),
            Err(NamespaceError::InvalidHash {
                source: ContentHashParseError::InvalidLength { found: 10 }
            })
        );
        assert_eq!(
            parse_deployed_name(
                "workflow$nested$0000000000000000000000000000000000000000000000000000000000000000"
            ),
            Err(NamespaceError::SeparatorInLogicalName)
        );
    }

    #[test]
    fn deployed_name_sets_for_different_hashes_are_disjoint() -> Result<(), crate::PackageError> {
        let beams = beam_set()?;
        let first = deployed_names(&beams, &hash(0x01));
        let second = deployed_names(&beams, &hash(0x02));

        assert!(first.is_disjoint(&second));
        Ok(())
    }

    #[test]
    fn same_logical_module_under_same_hash_is_idempotent() {
        let hash = hash(0x55);

        assert_eq!(
            deployed_name("order_workflow", &hash),
            deployed_name("order_workflow", &hash)
        );
    }

    #[test]
    fn deployed_names_follow_beam_set_canonical_order() -> Result<(), crate::PackageError> {
        let beams = beam_set()?;
        let names = deployed_names(&beams, &hash(0x09));
        let expected = BTreeSet::from([
            deployed_name("stdlib/list", &hash(0x09)),
            deployed_name("workflow/a", &hash(0x09)),
            deployed_name("workflow/b", &hash(0x09)),
        ]);

        assert_eq!(names, expected);
        Ok(())
    }
}
