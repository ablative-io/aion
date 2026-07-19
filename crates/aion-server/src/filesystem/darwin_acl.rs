//! Darwin extended-ACL validation for path-ambient backend ancestors.

use std::path::Path;

use exacl::{AclEntryKind, AclOption, Perm};
use uuid::Uuid;

use super::DataRootAncestorError;

fn same_user_principal(effective_user: &Uuid, ace_qualifier: &Uuid) -> bool {
    effective_user == ace_qualifier
}

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

    let qualifier_uuids = aion_darwin_acl::qualifier_uuids(component).map_err(|error| {
        DataRootAncestorError::new(
            component.to_path_buf(),
            format!(
                "could not read raw extended ACL qualifier UUIDs without following links: {error}"
            ),
        )
    })?;
    if qualifier_uuids.len() != entries.len() {
        return Err(DataRootAncestorError::new(
            component.to_path_buf(),
            format!(
                "could not correlate extended ACL entries with raw qualifier UUIDs: read {} \
                 entries but {} qualifiers",
                entries.len(),
                qualifier_uuids.len()
            ),
        ));
    }
    let effective_user_uuid = aion_darwin_acl::user_uuid(effective_uid).map_err(|error| {
        DataRootAncestorError::new(
            component.to_path_buf(),
            format!("could not resolve server euid {effective_uid} to a Darwin UUID: {error}"),
        )
    })?;

    let mutation_authority = Perm::EXECUTE
        | Perm::WRITE
        | Perm::APPEND
        | Perm::DELETE_CHILD
        | Perm::DELETE
        | Perm::WRITESECURITY
        | Perm::CHOWN;
    for (entry, qualifier_uuid) in entries.into_iter().zip(qualifier_uuids) {
        if !entry.allow || !entry.perms.intersects(mutation_authority) {
            continue;
        }

        let is_effective_user = entry.kind == AclEntryKind::User
            && same_user_principal(&effective_user_uuid, &qualifier_uuid);
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

#[cfg(test)]
pub(super) fn user_uuid_for_test(uid: u32) -> std::io::Result<Uuid> {
    aion_darwin_acl::user_uuid(uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_user_uuids_with_identical_display_names_compare_unequal() {
        struct RenderedPrincipal {
            display_name: &'static str,
            qualifier_uuid: Uuid,
        }

        let effective_user = RenderedPrincipal {
            display_name: "svc",
            qualifier_uuid: Uuid::from_bytes([0x01; 16]),
        };
        let different_user = RenderedPrincipal {
            display_name: "svc",
            qualifier_uuid: Uuid::from_bytes([0x02; 16]),
        };

        assert_eq!(effective_user.display_name, different_user.display_name);
        assert!(!same_user_principal(
            &effective_user.qualifier_uuid,
            &different_user.qualifier_uuid
        ));
    }

    #[test]
    fn identical_user_uuid_compares_equal() {
        let effective_user = Uuid::from_bytes([0x01; 16]);
        assert!(same_user_principal(&effective_user, &effective_user));
    }
}
