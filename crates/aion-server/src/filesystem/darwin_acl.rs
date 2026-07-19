//! Darwin extended-ACL validation for path-ambient backend ancestors.

use std::path::Path;

use aion_darwin_acl::{AclEntryKind, DecodedAclEntry, Permissions};
use uuid::Uuid;

use super::DataRootAncestorError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AceDecision {
    Ignore,
    AcceptEffectiveUser,
    Reject,
}

fn decide_ace(entry: &DecodedAclEntry, effective_user: &Uuid) -> AceDecision {
    let mutation_authority = Permissions::EXECUTE
        | Permissions::WRITE
        | Permissions::APPEND
        | Permissions::DELETE_CHILD
        | Permissions::DELETE
        | Permissions::WRITE_SECURITY
        | Permissions::CHANGE_OWNER;
    if !entry.allow || !entry.permissions.intersects(mutation_authority) {
        return AceDecision::Ignore;
    }

    if entry.kind == AclEntryKind::User && entry.qualifier_uuid == *effective_user {
        AceDecision::AcceptEffectiveUser
    } else {
        AceDecision::Reject
    }
}

/// Reject non-euid allow ACEs that can mutate an ambient pathname boundary.
pub(super) fn validate_extended_acl(
    component: &Path,
    effective_uid: u32,
) -> Result<(), DataRootAncestorError> {
    let entries = aion_darwin_acl::read_entries(component).map_err(|error| {
        DataRootAncestorError::new(
            component.to_path_buf(),
            format!(
                "could not read and decode one extended ACL snapshot without following links: \
                 {error}"
            ),
        )
    })?;
    let effective_user_uuid = aion_darwin_acl::user_uuid(effective_uid).map_err(|error| {
        DataRootAncestorError::new(
            component.to_path_buf(),
            format!("could not resolve server euid {effective_uid} to a Darwin UUID: {error}"),
        )
    })?;

    for entry in entries {
        if decide_ace(&entry, &effective_user_uuid) != AceDecision::Reject {
            continue;
        }
        let display_name = entry.display_name.as_deref().unwrap_or("<unresolved>");
        return Err(DataRootAncestorError::new(
            component.to_path_buf(),
            format!(
                "extended ACL allow ACE for `{display_name}` ({:?} {}) with permission mask \
                 {:#x} grants traversal, entry mutation, child deletion, or security mutation \
                 to a principal other than server euid {effective_uid}",
                entry.kind,
                entry.qualifier_uuid,
                entry.permissions.bits()
            ),
        ));
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

    fn mutating_allow(
        kind: AclEntryKind,
        display_name: Option<&str>,
        qualifier_uuid: Uuid,
    ) -> DecodedAclEntry {
        DecodedAclEntry {
            allow: true,
            kind,
            display_name: display_name.map(str::to_owned),
            qualifier_uuid,
            permissions: Permissions::WRITE_SECURITY,
        }
    }

    #[test]
    fn decision_accepts_the_euid_user_uuid() {
        let effective_user = Uuid::from_bytes([0x01; 16]);
        let entry = mutating_allow(AclEntryKind::User, Some("svc"), effective_user);

        assert_eq!(
            decide_ace(&entry, &effective_user),
            AceDecision::AcceptEffectiveUser
        );
    }

    #[test]
    fn decision_rejects_a_distinct_user_uuid_with_the_same_display_name() {
        let effective_user = Uuid::from_bytes([0x01; 16]);
        let effective_entry = mutating_allow(AclEntryKind::User, Some("svc"), effective_user);
        let foreign_entry = mutating_allow(
            AclEntryKind::User,
            effective_entry.display_name.as_deref(),
            Uuid::from_bytes([0x02; 16]),
        );

        assert_eq!(effective_entry.display_name, foreign_entry.display_name);
        assert_eq!(
            decide_ace(&foreign_entry, &effective_user),
            AceDecision::Reject
        );
    }

    #[test]
    fn decision_rejects_a_group_even_when_its_uuid_matches_the_euid() {
        let effective_user = Uuid::from_bytes([0x01; 16]);
        let entry = mutating_allow(AclEntryKind::Group, Some("svc"), effective_user);

        assert_eq!(decide_ace(&entry, &effective_user), AceDecision::Reject);
    }

    #[test]
    fn decision_rejects_an_unknown_unobtainable_identity() {
        let effective_user = Uuid::from_bytes([0x01; 16]);
        let entry = mutating_allow(AclEntryKind::Unknown, None, effective_user);

        assert_eq!(decide_ace(&entry, &effective_user), AceDecision::Reject);
    }

    #[test]
    fn decision_ignores_deny_and_non_mutating_entries() {
        let effective_user = Uuid::from_bytes([0x01; 16]);
        let mut entry = mutating_allow(AclEntryKind::Unknown, None, Uuid::from_bytes([0x02; 16]));
        entry.allow = false;
        assert_eq!(decide_ace(&entry, &effective_user), AceDecision::Ignore);

        entry.allow = true;
        entry.permissions = Permissions::READ;
        assert_eq!(decide_ace(&entry, &effective_user), AceDecision::Ignore);
    }
}
