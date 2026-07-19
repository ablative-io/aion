//! Minimal owned-value wrappers around Darwin's raw ACL qualifier APIs.
//!
//! This crate is the sole local unsafe leaf for the path-ambient ACL gate. It
//! copies native UUID qualifiers into owned [`Uuid`] values and releases every
//! allocation before returning to safe callers.

#![cfg(target_os = "macos")]

use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::ptr;

use uuid::Uuid;

type Acl = *mut c_void;
type AclEntry = *mut c_void;

const ACL_TYPE_EXTENDED: c_uint = 0x100;
const ACL_FIRST_ENTRY: c_int = 0;
const ACL_NEXT_ENTRY: c_int = -1;

unsafe extern "C" {
    fn acl_get_link_np(path: *const c_char, acl_type: c_uint) -> Acl;
    fn acl_get_entry(acl: Acl, entry_id: c_int, entry: *mut AclEntry) -> c_int;
    fn acl_get_qualifier(entry: AclEntry) -> *mut c_void;
    fn acl_free(object: *mut c_void) -> c_int;
    fn mbr_uid_to_uuid(uid: u32, uuid: *mut u8) -> c_int;
}

struct OwnedAcl(Acl);

impl Drop for OwnedAcl {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a live object returned by `acl_get_link_np`, and
        // this wrapper is its sole owner.
        let _ = unsafe { acl_free(self.0) };
    }
}

/// Resolve a Darwin user ID to its membership UUID.
///
/// # Errors
///
/// Returns the Darwin membership API error when the UID has no resolvable UUID.
pub fn user_uuid(uid: u32) -> io::Result<Uuid> {
    let mut bytes = [0_u8; 16];
    // SAFETY: `bytes` is a writable 16-byte uuid_t destination for the duration
    // of the call; mbr_uid_to_uuid does not retain the pointer.
    let result = unsafe { mbr_uid_to_uuid(uid, bytes.as_mut_ptr()) };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    Ok(Uuid::from_bytes(bytes))
}

/// Read every raw UUID qualifier from a path's extended ACL without following
/// the final path component.
///
/// Qualifiers retain native ACL entry order so callers can correlate them with
/// an ordered safe ACL decode. A present path without an extended ACL yields an
/// empty vector.
///
/// # Errors
///
/// Returns an I/O error when the path or any native qualifier cannot be read or
/// when a native allocation cannot be released.
pub fn qualifier_uuids(path: &Path) -> io::Result<Vec<Uuid>> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `c_path` is NUL-terminated and remains live for the call. The
    // returned ACL is owned and released by `OwnedAcl` below.
    let acl = unsafe { acl_get_link_np(c_path.as_ptr(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound && path.symlink_metadata().is_ok() {
            return Ok(Vec::new());
        }
        return Err(error);
    }
    let acl = OwnedAcl(acl);

    let mut qualifiers = Vec::new();
    let mut entry = ptr::null_mut();
    let mut entry_id = ACL_FIRST_ENTRY;
    loop {
        // SAFETY: the ACL remains live in `acl`, and `entry` is a writable
        // out-pointer. Darwin returns zero for an entry and nonzero at end.
        if unsafe { acl_get_entry(acl.0, entry_id, &raw mut entry) } != 0 {
            break;
        }
        if entry.is_null() {
            return Err(io::Error::other("acl_get_entry returned a null entry"));
        }

        // SAFETY: Darwin allocates one uuid_t qualifier for a live native entry.
        // Copy it before releasing that allocation with acl_free.
        let qualifier = unsafe { acl_get_qualifier(entry) };
        if qualifier.is_null() {
            return Err(io::Error::last_os_error());
        }
        let bytes = unsafe { ptr::read_unaligned(qualifier.cast::<[u8; 16]>()) };
        // SAFETY: `qualifier` is exactly the allocation returned above and has
        // not previously been released.
        if unsafe { acl_free(qualifier) } != 0 {
            return Err(io::Error::last_os_error());
        }
        qualifiers.push(Uuid::from_bytes(bytes));
        entry_id = ACL_NEXT_ENTRY;
    }
    Ok(qualifiers)
}
