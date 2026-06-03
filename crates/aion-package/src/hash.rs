//! Content-hash computation over the canonical beam set.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};

use crate::BeamSet;

const DIGEST_LEN: usize = 32;
const TEXT_LEN: usize = DIGEST_LEN * 2;

/// A SHA-256 content hash computed over the canonical BEAM module set only.
///
/// The digest is computed from each module's logical name and exact `.beam`
/// bytes in [`BeamSet`] canonical order. Archive bytes, compression settings,
/// timestamps, and optional source inclusion are never part of this hash, so the
/// same compiled modules produce the same package version across deterministic
/// and non-deterministic archive representations.
///
/// Its stable textual form is 64 lowercase hexadecimal characters. That text is
/// the package version identifier stored in the manifest and the hash component
/// embedded in namespaced deployed module names; it contains only `0-9a-f`,
/// which is safe for a BEAM module-name component.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ContentHash([u8; DIGEST_LEN]);

impl ContentHash {
    /// Creates a content hash from raw SHA-256 digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; DIGEST_LEN]) -> Self {
        Self(bytes)
    }

    /// Returns the raw SHA-256 digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; DIGEST_LEN] {
        &self.0
    }
}

/// Errors produced when parsing a [`ContentHash`] textual form.
#[derive(thiserror::Error, Clone, Debug, PartialEq, Eq)]
pub enum ContentHashParseError {
    /// The text was not exactly 64 ASCII hexadecimal characters.
    #[error("content hash text must be 64 lowercase hexadecimal characters, found {found} bytes")]
    InvalidLength {
        /// Number of bytes found in the supplied text.
        found: usize,
    },

    /// The text contained a character outside lowercase hexadecimal.
    #[error("content hash text contains non-lowercase-hex byte 0x{byte:02x} at byte index {index}")]
    InvalidCharacter {
        /// Byte index of the invalid character.
        index: usize,
        /// Invalid byte found at `index`.
        byte: u8,
    },
}

/// Computes the package version hash over the canonical BEAM set only.
///
/// The SHA-256 algorithm is mandated by the `.aion` format contract so packers
/// and loaders on different hosts agree. Each module contributes its logical
/// name and exact bytes in [`BeamSet`] canonical order, with each field framed by
/// an eight-byte big-endian length prefix. This unambiguous framing prevents a
/// shifted name/body boundary from producing the same digest.
#[must_use]
pub fn content_hash(beams: &BeamSet) -> ContentHash {
    let mut digest = Sha256::new();

    for module in beams.iter() {
        update_framed(&mut digest, module.name().as_bytes());
        update_framed(&mut digest, module.bytes());
    }

    ContentHash(digest.finalize().into())
}

fn update_framed(digest: &mut Sha256, bytes: &[u8]) {
    let length = bytes.len() as u64;
    digest.update(length.to_be_bytes().as_slice());
    digest.update(bytes);
}

impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(formatter, "{byte:02x}")?;
        }

        Ok(())
    }
}

impl FromStr for ContentHash {
    type Err = ContentHashParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let bytes = text.as_bytes();
        if bytes.len() != TEXT_LEN {
            return Err(ContentHashParseError::InvalidLength { found: bytes.len() });
        }

        let mut digest = [0_u8; DIGEST_LEN];
        for (index, pair) in bytes.chunks_exact(2).enumerate() {
            let high_index = index * 2;
            let low_index = high_index + 1;
            digest[index] = (hex_value(pair[0], high_index)? << 4) | hex_value(pair[1], low_index)?;
        }

        Ok(Self(digest))
    }
}

fn hex_value(byte: u8, index: usize) -> Result<u8, ContentHashParseError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(ContentHashParseError::InvalidCharacter { index, byte }),
    }
}

impl Serialize for ContentHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(ContentHashVisitor)
    }
}

struct ContentHashVisitor;

impl de::Visitor<'_> for ContentHashVisitor {
    type Value = ContentHash;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a 64-character lowercase hexadecimal SHA-256 content hash")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        ContentHash::from_str(value).map_err(E::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{ContentHash, content_hash};
    use crate::{BeamModule, BeamSet, PackageError};

    #[test]
    fn content_hash_is_independent_of_insertion_order() -> Result<(), PackageError> {
        let first = BeamSet::new(vec![
            BeamModule::new("workflow/c", vec![3]),
            BeamModule::new("workflow/a", vec![1]),
            BeamModule::new("workflow/b", vec![2]),
        ])?;
        let second = BeamSet::new(vec![
            BeamModule::new("workflow/b", vec![2]),
            BeamModule::new("workflow/c", vec![3]),
            BeamModule::new("workflow/a", vec![1]),
        ])?;

        assert_eq!(content_hash(&first), content_hash(&second));

        Ok(())
    }

    #[test]
    fn content_hash_changes_when_a_module_byte_changes() -> Result<(), PackageError> {
        let original = BeamSet::new(vec![
            BeamModule::new("workflow/a", vec![1, 2, 3]),
            BeamModule::new("workflow/b", vec![4, 5, 6]),
        ])?;
        let changed = BeamSet::new(vec![
            BeamModule::new("workflow/a", vec![1, 2, 3]),
            BeamModule::new("workflow/b", vec![4, 5, 7]),
        ])?;

        assert_ne!(content_hash(&original), content_hash(&changed));

        Ok(())
    }

    #[test]
    fn content_hash_changes_when_a_module_name_changes() -> Result<(), PackageError> {
        let original = BeamSet::new(vec![BeamModule::new("workflow/a", vec![1, 2, 3])])?;
        let renamed = BeamSet::new(vec![BeamModule::new("workflow/renamed", vec![1, 2, 3])])?;

        assert_ne!(content_hash(&original), content_hash(&renamed));

        Ok(())
    }

    #[test]
    fn content_hash_framing_prevents_name_bytes_boundary_ambiguity() -> Result<(), PackageError> {
        let first = BeamSet::new(vec![BeamModule::new("ab", b"c".to_vec())])?;
        let second = BeamSet::new(vec![BeamModule::new("a", b"bc".to_vec())])?;

        assert_ne!(content_hash(&first), content_hash(&second));

        Ok(())
    }

    #[test]
    fn content_hash_text_round_trips() -> Result<(), PackageError> {
        let beams = BeamSet::new(vec![BeamModule::new("workflow/a", vec![0, 1, 2, 255])])?;
        let hash = content_hash(&beams);
        let text = hash.to_string();
        let parsed = text.parse::<ContentHash>();

        assert_eq!(text.len(), 64);
        assert!(
            text.bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        );
        assert_eq!(parsed, Ok(hash));

        Ok(())
    }

    #[test]
    fn content_hash_rejects_uppercase_text() {
        let text = "A000000000000000000000000000000000000000000000000000000000000000";

        assert!(text.parse::<ContentHash>().is_err());
    }
}
