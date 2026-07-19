//! Darwin extended-ACL validation for path-ambient backend ancestors.

use std::ffi::OsStr;
use std::path::Path;

use exacl::{AclEntryKind, AclOption, Perm};

use super::DataRootAncestorError;

/// Reject non-euid allow ACEs that can mutate an ambient pathname boundary.
pub(super) fn validate_extended_acl(
    component: &Path,
    effective_uid: u32,
) -> Result<(), DataRootAncestorError> {
    // On Darwin exacl implements SYMLINK_ACL with acl_get_link_np. Although the
    // caller has already established that this component is a directory, using
    // the link API preserves the gate's no-follow rule if its name races.
    let entries = exacl::getfacl(component, AclOption::SYMLINK_ACL).map_err(|error| {
        DataRootAncestorError::new(
            component.to_path_buf(),
            format!("could not read or interpret extended ACL without following links: {error}"),
        )
    })?;

    let mutation_authority = Perm::EXECUTE
        | Perm::WRITE
        | Perm::APPEND
        | Perm::DELETE_CHILD
        | Perm::DELETE
        | Perm::WRITESECURITY
        | Perm::CHOWN;
    for entry in entries {
        if !entry.allow || !entry.perms.intersects(mutation_authority) {
            continue;
        }

        let is_effective_user = if entry.kind == AclEntryKind::User {
            let effective_user = users::get_user_by_uid(effective_uid).ok_or_else(|| {
                DataRootAncestorError::new(
                    component.to_path_buf(),
                    format!(
                        "could not interpret extended ACL allow ACE `{entry}`: \
                         server euid {effective_uid} has no resolvable account name"
                    ),
                )
            })?;
            effective_user.name() == OsStr::new(&entry.name)
        } else {
            false
        };
        if !is_effective_user {
            return Err(DataRootAncestorError::new(
                component.to_path_buf(),
                format!(
                    "extended ACL allow ACE `{entry}` grants traversal, entry mutation, child \
                     deletion, or security mutation to a principal other than server euid \
                     {effective_uid}"
                ),
            ));
        }
    }
    Ok(())
}
