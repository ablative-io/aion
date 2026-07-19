use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::ptr;

use uuid::Uuid;

use crate::identity::resolve_identity;
use crate::{DecodedAclEntry, Permissions};

type Acl = *mut c_void;
type AclEntry = *mut c_void;
type AclPermset = *mut c_void;

const ACL_TYPE_EXTENDED: c_uint = 0x100;
const ACL_FIRST_ENTRY: c_int = 0;
const ACL_NEXT_ENTRY: c_int = -1;
const ACL_EXTENDED_ALLOW: c_uint = 1;
const ACL_EXTENDED_DENY: c_uint = 2;
const KNOWN_PERMISSIONS: [Permissions; 14] = [
    Permissions::READ,
    Permissions::WRITE,
    Permissions::EXECUTE,
    Permissions::DELETE,
    Permissions::APPEND,
    Permissions::DELETE_CHILD,
    Permissions::READ_ATTRIBUTES,
    Permissions::WRITE_ATTRIBUTES,
    Permissions::READ_EXTATTRIBUTES,
    Permissions::WRITE_EXTATTRIBUTES,
    Permissions::READ_SECURITY,
    Permissions::WRITE_SECURITY,
    Permissions::CHANGE_OWNER,
    Permissions::SYNCHRONIZE,
];

unsafe extern "C" {
    fn acl_get_link_np(path: *const c_char, acl_type: c_uint) -> Acl;
    fn acl_get_entry(acl: Acl, entry_id: c_int, entry: *mut AclEntry) -> c_int;
    fn acl_get_qualifier(entry: AclEntry) -> *mut c_void;
    fn acl_get_tag_type(entry: AclEntry, tag: *mut c_uint) -> c_int;
    fn acl_get_permset(entry: AclEntry, permset: *mut AclPermset) -> c_int;
    fn acl_get_perm_np(permset: AclPermset, permission: c_uint) -> c_int;
    fn acl_free(object: *mut c_void) -> c_int;
}

struct OwnedAcl(Acl);

impl Drop for OwnedAcl {
    fn drop(&mut self) {
        // SAFETY: `self.0` is live and this wrapper is its sole owner.
        let _ = unsafe { acl_free(self.0) };
    }
}

pub(super) fn read_entries(path: &Path) -> io::Result<Vec<DecodedAclEntry>> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `c_path` is NUL-terminated and live. `OwnedAcl` owns the result.
    let acl = unsafe { acl_get_link_np(c_path.as_ptr(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound && path.symlink_metadata().is_ok() {
            return Ok(Vec::new());
        }
        return Err(error);
    }
    let acl = OwnedAcl(acl);

    let mut entries = Vec::new();
    let mut entry = ptr::null_mut();
    let mut entry_id = ACL_FIRST_ENTRY;
    loop {
        // SAFETY: `acl` is live and `entry` is a writable out-pointer.
        if unsafe { acl_get_entry(acl.0, entry_id, &raw mut entry) } != 0 {
            break;
        }
        if entry.is_null() {
            return Err(io::Error::other("acl_get_entry returned a null entry"));
        }
        entries.push(decode_entry(entry)?);
        entry_id = ACL_NEXT_ENTRY;
    }
    Ok(entries)
}

fn decode_entry(entry: AclEntry) -> io::Result<DecodedAclEntry> {
    let allow = read_allow(entry)?;
    let permissions = read_permissions(entry)?;
    let qualifier_uuid = read_qualifier(entry)?;
    let (kind, display_name) = resolve_identity(&qualifier_uuid)?;
    Ok(DecodedAclEntry {
        allow,
        kind,
        display_name,
        qualifier_uuid,
        permissions,
    })
}

fn read_allow(entry: AclEntry) -> io::Result<bool> {
    let mut tag = 0;
    // SAFETY: `entry` is live and `tag` is a writable, unretained output.
    if unsafe { acl_get_tag_type(entry, &raw mut tag) } != 0 {
        return Err(last_native_error("acl_get_tag_type"));
    }
    match tag {
        ACL_EXTENDED_ALLOW => Ok(true),
        ACL_EXTENDED_DENY => Ok(false),
        _ => Err(io::Error::other(format!(
            "acl_get_tag_type returned unsupported tag {tag}"
        ))),
    }
}

fn read_permissions(entry: AclEntry) -> io::Result<Permissions> {
    let mut permset = ptr::null_mut();
    // SAFETY: `entry` is live and `permset` receives a snapshot-borrowed pointer.
    if unsafe { acl_get_permset(entry, &raw mut permset) } != 0 {
        return Err(last_native_error("acl_get_permset"));
    }
    if permset.is_null() {
        return Err(io::Error::other("acl_get_permset returned a null set"));
    }

    let mut permissions = Permissions::EMPTY;
    for permission in KNOWN_PERMISSIONS {
        // SAFETY: `permset` is live and the permission value is documented.
        match unsafe { acl_get_perm_np(permset, permission.bits()) } {
            0 => {}
            1 => permissions |= permission,
            _ => return Err(last_native_error("acl_get_perm_np")),
        }
    }
    Ok(permissions)
}

fn read_qualifier(entry: AclEntry) -> io::Result<Uuid> {
    // SAFETY: Darwin allocates one uuid_t for a live native entry.
    let qualifier = unsafe { acl_get_qualifier(entry) };
    if qualifier.is_null() {
        return Err(last_native_error("acl_get_qualifier"));
    }
    // SAFETY: the qualifier is a 16-byte uuid_t and may be under-aligned.
    let bytes = unsafe { ptr::read_unaligned(qualifier.cast::<[u8; 16]>()) };
    // SAFETY: this is the live allocation returned immediately above.
    if unsafe { acl_free(qualifier) } != 0 {
        return Err(last_native_error("acl_free qualifier"));
    }
    Ok(Uuid::from_bytes(bytes))
}

fn last_native_error(function: &str) -> io::Error {
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(0) {
        io::Error::other(format!("{function} failed without setting errno"))
    } else {
        io::Error::new(error.kind(), format!("{function}: {error}"))
    }
}
