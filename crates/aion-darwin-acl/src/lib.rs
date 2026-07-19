//! Owned-value wrappers around Darwin's native ACL and membership APIs.
//!
//! This crate is the sole local unsafe leaf for the path-ambient ACL gate. It
//! reads and decodes each ACL from one owned native snapshot, copies qualifier
//! UUIDs into owned values, and releases every native allocation before
//! returning to safe callers.

#![cfg(target_os = "macos")]

use std::io;
use std::ops::{BitOr, BitOrAssign};
use std::path::Path;

use uuid::Uuid;

mod identity;
mod native;

/// The resolved Darwin membership kind of an ACL qualifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AclEntryKind {
    /// A user qualifier.
    User,
    /// A group qualifier.
    Group,
    /// A qualifier whose successful membership lookup returned an unsupported kind.
    Unknown,
}

/// Darwin extended-ACL permission bits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Permissions(u32);

impl Permissions {
    /// No permissions.
    pub const EMPTY: Self = Self(0);
    /// Read file data or list a directory.
    pub const READ: Self = Self(1 << 1);
    /// Write file data or add a file to a directory.
    pub const WRITE: Self = Self(1 << 2);
    /// Execute a file or search a directory.
    pub const EXECUTE: Self = Self(1 << 3);
    /// Delete the ACL-protected object.
    pub const DELETE: Self = Self(1 << 4);
    /// Append file data or add a subdirectory.
    pub const APPEND: Self = Self(1 << 5);
    /// Delete a directory child.
    pub const DELETE_CHILD: Self = Self(1 << 6);
    /// Read attributes.
    pub const READ_ATTRIBUTES: Self = Self(1 << 7);
    /// Write attributes.
    pub const WRITE_ATTRIBUTES: Self = Self(1 << 8);
    /// Read extended attributes.
    pub const READ_EXTATTRIBUTES: Self = Self(1 << 9);
    /// Write extended attributes.
    pub const WRITE_EXTATTRIBUTES: Self = Self(1 << 10);
    /// Read security information.
    pub const READ_SECURITY: Self = Self(1 << 11);
    /// Write security information.
    pub const WRITE_SECURITY: Self = Self(1 << 12);
    /// Change ownership.
    pub const CHANGE_OWNER: Self = Self(1 << 13);
    /// Synchronize access.
    pub const SYNCHRONIZE: Self = Self(1 << 20);

    /// Return the raw Darwin permission mask.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Return whether this set and `other` share at least one permission.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

impl BitOr for Permissions {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for Permissions {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// One completely decoded entry from a single owned native ACL snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedAclEntry {
    /// Whether this is an allow entry (`false` means deny).
    pub allow: bool,
    /// The resolved membership kind of the qualifier.
    pub kind: AclEntryKind,
    /// The display name resolved from the qualifier's UID or GID, when available.
    pub display_name: Option<String>,
    /// The exact raw UUID copied from this native entry's qualifier.
    pub qualifier_uuid: Uuid,
    /// The permission set decoded from this same native entry.
    pub permissions: Permissions,
}

/// Resolve a Darwin user ID to its membership UUID.
///
/// # Errors
///
/// Returns the Darwin membership API error when the UID has no resolvable UUID.
pub fn user_uuid(uid: u32) -> io::Result<Uuid> {
    identity::user_uuid(uid)
}

/// Read and completely decode a path's extended ACL without following its final
/// component.
///
/// The function obtains the ACL with exactly one `acl_get_link_np` call. Every
/// entry's tag, permissions, identity kind, display name, and raw qualifier UUID
/// are derived from that one owned native snapshot. A present path without an
/// extended ACL yields an empty vector.
///
/// # Errors
///
/// Returns an I/O error when the path, an ACL entry, a qualifier, a permission
/// set, or a membership identity cannot be decoded, or when a qualifier
/// allocation cannot be released.
pub fn read_entries(path: &Path) -> io::Result<Vec<DecodedAclEntry>> {
    native::read_entries(path)
}
