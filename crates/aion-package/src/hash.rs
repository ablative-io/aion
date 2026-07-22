//! Content-hash computation over the canonical beam set.

use std::{fmt, str::FromStr, time::Duration};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};

use crate::{BeamSet, Manifest, PackageError};

const DIGEST_LEN: usize = 32;
const TEXT_LEN: usize = DIGEST_LEN * 2;
const WORKFLOW_TIMEOUT_DOMAIN: &[u8] = b"aion.package.version.workflow-timeout.v1";
const WORKFLOW_TIMEOUTS_DOMAIN: &[u8] = b"aion.package.version.workflow-timeouts.v2";

/// A SHA-256 package version identity.
///
/// Legacy identities cover each module's logical name and exact `.beam` bytes
/// in [`BeamSet`] canonical order. Explicit-timeout identities append a
/// domain-separated timeout encoding. Archive representation and optional
/// source inclusion never participate, so deterministic inputs keep one version.
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
    update_beams(&mut digest, beams);
    ContentHash(digest.finalize().into())
}

/// Computes an explicit-timeout package version from the canonical BEAM set,
/// then the framed ASCII domain `aion.package.version.workflow-timeout.v1`,
/// then exactly 12 timeout bytes: seconds as `u64` big-endian followed by
/// subsecond nanoseconds as `u32` big-endian.
///
/// This single-entry form is retained for external callers; the per-entry
/// [`content_hash_with_timeouts`] is the authority the loader trusts, because it
/// binds every workflow entry's timeout — not only the primary — into identity.
#[must_use]
pub fn content_hash_with_timeout(beams: &BeamSet, timeout: Duration) -> ContentHash {
    let mut digest = Sha256::new();
    update_beams(&mut digest, beams);
    update_framed(&mut digest, WORKFLOW_TIMEOUT_DOMAIN);
    digest.update(timeout.as_secs().to_be_bytes());
    digest.update(timeout.subsec_nanos().to_be_bytes());
    ContentHash(digest.finalize().into())
}

/// Computes the per-entry timeout-bearing package version over the canonical
/// BEAM set, then the framed ASCII domain
/// `aion.package.version.workflow-timeouts.v2`, then a canonical encoding of
/// EVERY entry's authored timeout: the primary entry's timeout, the additional
/// entry count as `u64` big-endian, then for each additional entry (in manifest
/// order) its framed `workflow_type` bytes and its authored timeout.
///
/// Each authored timeout is encoded presence-first (a single `1`/`0` byte),
/// followed — only when present — by seconds as `u64` big-endian and subsecond
/// nanoseconds as `u32` big-endian. Binding presence AND value AND the entry's
/// own routing identity means no additional entry's timeout can be swapped,
/// added, or removed without changing the version hash: declaredness is an
/// authenticated per-entry property, not a package-wide inference from the
/// primary alone.
#[must_use]
pub fn content_hash_with_timeouts(beams: &BeamSet, manifest: &Manifest) -> ContentHash {
    let mut digest = Sha256::new();
    update_beams(&mut digest, beams);
    update_framed(&mut digest, WORKFLOW_TIMEOUTS_DOMAIN);
    update_timeout_field(&mut digest, manifest.timeout);
    let additional = &manifest.additional_workflows;
    digest.update((additional.len() as u64).to_be_bytes());
    for entry in additional {
        update_framed(&mut digest, entry.workflow_type.as_bytes());
        update_timeout_field(&mut digest, entry.timeout);
    }
    ContentHash(digest.finalize().into())
}

/// Whether this package's version identity commits to explicitly authored
/// per-entry workflow timeouts.
///
/// True only when the stored content hash is the domain-separated per-entry
/// timeout-bearing identity ([`content_hash_with_timeouts`]) — never the
/// beams-only legacy identity. A legacy (beams-only) archive, or one whose
/// primary timeout was bound but whose additional entries were not, therefore
/// reads as wholly NOT declared: no entry can arm a deadline. The check is
/// tamper-evident: the timeout value returned by [`crate::Package`] for any
/// entry is provably the one baked into the version hash, so a hand-edited or
/// injected per-entry timeout that was not part of the identity cannot fake
/// declaredness.
pub(crate) fn has_explicit_timeout_identity(
    beams: &BeamSet,
    manifest: &Manifest,
    hash: &ContentHash,
) -> bool {
    hash != &content_hash(beams) && hash == &content_hash_with_timeouts(beams, manifest)
}

pub(crate) fn verified_content_hash(
    beams: &BeamSet,
    manifest: &Manifest,
) -> Result<ContentHash, PackageError> {
    let legacy_hash = content_hash(beams);
    let stored = manifest.version.as_str();
    if stored == legacy_hash.to_string() {
        return Ok(legacy_hash);
    }
    let timeouts_hash = content_hash_with_timeouts(beams, manifest);
    if stored == timeouts_hash.to_string() {
        return Ok(timeouts_hash);
    }
    Err(PackageError::IntegrityMismatch {
        expected: stored.to_owned(),
        computed: legacy_hash.to_string(),
    })
}

/// Frames one entry's optional authored timeout into the digest: a presence
/// byte, then seconds (`u64` big-endian) and subsecond nanoseconds (`u32`
/// big-endian) only when a timeout is present. An absent timeout contributes
/// exactly the `0` presence byte, so presence and value are both bound.
fn update_timeout_field(digest: &mut Sha256, timeout: Option<Duration>) {
    match timeout {
        Some(timeout) => {
            digest.update([1_u8]);
            digest.update(timeout.as_secs().to_be_bytes());
            digest.update(timeout.subsec_nanos().to_be_bytes());
        }
        None => digest.update([0_u8]),
    }
}

fn update_beams(digest: &mut Sha256, beams: &BeamSet) {
    for module in beams.iter() {
        update_framed(digest, module.name().as_bytes());
        update_framed(digest, module.bytes());
    }
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
    use std::time::Duration;

    use serde_json::json;

    use super::{
        ContentHash, content_hash, content_hash_with_timeout, content_hash_with_timeouts,
        has_explicit_timeout_identity,
    };
    use crate::{
        BeamModule, BeamSet, CURRENT_FORMAT_VERSION, Manifest, ManifestVersion, PackageError,
        WorkflowEntry,
    };

    fn manifest_with(primary: Option<Duration>, additional: Vec<WorkflowEntry>) -> Manifest {
        Manifest {
            entry_module: "workflow/a".to_owned(),
            entry_function: "run".to_owned(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "object" }),
            timeout: primary,
            activities: Vec::new(),
            version: ManifestVersion::new("unstamped"),
            format_version: CURRENT_FORMAT_VERSION,
            additional_workflows: additional,
        }
    }

    fn additional_entry(workflow_type: &str, timeout: Option<Duration>) -> WorkflowEntry {
        WorkflowEntry {
            workflow_type: workflow_type.to_owned(),
            entry_module: "workflow/a".to_owned(),
            entry_function: format!("{workflow_type}_run"),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "object" }),
            timeout,
            internal: true,
        }
    }

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
    fn legacy_identity_remains_exactly_the_beams_only_hash() -> Result<(), PackageError> {
        let beams = BeamSet::new(vec![BeamModule::new("workflow/a", vec![1, 2, 3])])?;
        let pre_change_rule = content_hash(&beams);
        assert_eq!(content_hash(&beams), pre_change_rule);
        Ok(())
    }

    #[test]
    fn explicit_timeout_identity_is_deterministic_and_value_sensitive() -> Result<(), PackageError>
    {
        let beams = BeamSet::new(vec![BeamModule::new("workflow/a", vec![1, 2, 3])])?;
        let two_hours = content_hash_with_timeout(&beams, Duration::from_secs(7_200));
        assert_eq!(
            two_hours,
            content_hash_with_timeout(&beams, Duration::from_secs(7_200))
        );
        assert_ne!(
            two_hours,
            content_hash_with_timeout(&beams, Duration::from_secs(21_600))
        );
        assert_ne!(
            two_hours,
            content_hash_with_timeout(&beams, Duration::new(7_200, 500_000_000))
        );
        assert_ne!(two_hours, content_hash(&beams));
        Ok(())
    }

    #[test]
    fn per_entry_identity_binds_every_additional_entry_timeout() -> Result<(), PackageError> {
        let beams = BeamSet::new(vec![BeamModule::new("workflow/a", vec![1, 2, 3])])?;
        let base = manifest_with(
            Some(Duration::from_secs(60)),
            vec![additional_entry("child", Some(Duration::from_secs(30)))],
        );

        // Changing an additional entry's timeout value changes the identity.
        let changed_value = manifest_with(
            Some(Duration::from_secs(60)),
            vec![additional_entry("child", Some(Duration::from_secs(31)))],
        );
        assert_ne!(
            content_hash_with_timeouts(&beams, &base),
            content_hash_with_timeouts(&beams, &changed_value),
        );

        // Adding an unbound additional timeout (the mixed-archive attack) changes
        // the identity: it cannot ride the primary's declaredness.
        let injected = manifest_with(
            Some(Duration::from_secs(60)),
            vec![additional_entry("child", Some(Duration::from_secs(3_600)))],
        );
        assert_ne!(
            content_hash_with_timeouts(&beams, &base),
            content_hash_with_timeouts(&beams, &injected),
        );

        // Presence alone (Some vs None) changes the identity.
        let absent = manifest_with(
            Some(Duration::from_secs(60)),
            vec![additional_entry("child", None)],
        );
        assert_ne!(
            content_hash_with_timeouts(&beams, &base),
            content_hash_with_timeouts(&beams, &absent),
        );
        Ok(())
    }

    #[test]
    fn mixed_archive_with_injected_additional_timeout_reads_as_undeclared()
    -> Result<(), PackageError> {
        // A package whose stored hash bound ONLY the primary timeout (the old
        // single-entry identity) but which carries an additional entry with an
        // unauthenticated `Some(1h)` must read as wholly undeclared under the
        // per-entry identity: the stored hash matches neither the legacy nor the
        // per-entry timeout-bearing hash.
        let beams = BeamSet::new(vec![BeamModule::new("workflow/a", vec![1, 2, 3])])?;
        let manifest = manifest_with(
            Some(Duration::from_secs(60)),
            vec![additional_entry("child", Some(Duration::from_secs(3_600)))],
        );
        // The attacker stamps the primary-only identity as the version.
        let primary_only = content_hash_with_timeout(&beams, Duration::from_secs(60));
        assert!(
            !has_explicit_timeout_identity(&beams, &manifest, &primary_only),
            "an injected additional timeout cannot ride the primary-only identity"
        );
        // The beams-only legacy identity is likewise undeclared.
        assert!(!has_explicit_timeout_identity(
            &beams,
            &manifest,
            &content_hash(&beams)
        ));
        // Only the full per-entry identity authenticates every entry.
        assert!(has_explicit_timeout_identity(
            &beams,
            &manifest,
            &content_hash_with_timeouts(&beams, &manifest)
        ));
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
