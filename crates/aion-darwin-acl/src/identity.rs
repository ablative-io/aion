use std::ffi::CStr;
use std::io;
use std::mem::MaybeUninit;
use std::ptr;

use uuid::Uuid;

use crate::AclEntryKind;

const ID_TYPE_UID: libc::c_int = 0;
const ID_TYPE_GID: libc::c_int = 1;

unsafe extern "C" {
    fn mbr_uid_to_uuid(uid: u32, uuid: *mut u8) -> libc::c_int;
    fn mbr_uuid_to_id(uuid: *const u8, id: *mut u32, id_type: *mut libc::c_int) -> libc::c_int;
}

pub(super) fn user_uuid(uid: u32) -> io::Result<Uuid> {
    let mut bytes = [0_u8; 16];
    // SAFETY: `bytes` is a writable uuid_t and the pointer is not retained.
    let result = unsafe { mbr_uid_to_uuid(uid, bytes.as_mut_ptr()) };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    Ok(Uuid::from_bytes(bytes))
}

pub(super) fn resolve_identity(uuid: &Uuid) -> io::Result<(AclEntryKind, Option<String>)> {
    let mut id = 0_u32;
    let mut id_type = -1;
    // SAFETY: all three buffers are live and the call retains no pointers.
    let result = unsafe { mbr_uuid_to_id(uuid.as_bytes().as_ptr(), &raw mut id, &raw mut id_type) };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    Ok(match id_type {
        ID_TYPE_UID => (AclEntryKind::User, user_name(id)?),
        ID_TYPE_GID => (AclEntryKind::Group, group_name(id)?),
        _ => (AclEntryKind::Unknown, None),
    })
}

fn user_name(uid: u32) -> io::Result<Option<String>> {
    let mut record = MaybeUninit::<libc::passwd>::uninit();
    let mut buffer = lookup_buffer(libc::_SC_GETPW_R_SIZE_MAX)?;
    let mut result = ptr::null_mut();
    // SAFETY: the record, buffer, and result are writable for the entire call.
    let error = unsafe {
        libc::getpwuid_r(
            uid,
            record.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut result,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    if result.is_null() {
        return Ok(None);
    }
    // SAFETY: success with a non-null result initialized the record.
    let name = unsafe { (*result).pw_name };
    if name.is_null() {
        return Err(io::Error::other("getpwuid_r returned a null account name"));
    }
    // SAFETY: `name` points into `buffer`, which remains live while it is copied.
    let name = unsafe { CStr::from_ptr(name) };
    Ok(Some(name.to_string_lossy().into_owned()))
}

fn group_name(gid: u32) -> io::Result<Option<String>> {
    let mut record = MaybeUninit::<libc::group>::uninit();
    let mut buffer = lookup_buffer(libc::_SC_GETGR_R_SIZE_MAX)?;
    let mut result = ptr::null_mut();
    // SAFETY: the record, buffer, and result are writable for the entire call.
    let error = unsafe {
        libc::getgrgid_r(
            gid,
            record.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut result,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    if result.is_null() {
        return Ok(None);
    }
    // SAFETY: success with a non-null result initialized the record.
    let name = unsafe { (*result).gr_name };
    if name.is_null() {
        return Err(io::Error::other("getgrgid_r returned a null group name"));
    }
    // SAFETY: `name` points into `buffer`, which remains live while it is copied.
    let name = unsafe { CStr::from_ptr(name) };
    Ok(Some(name.to_string_lossy().into_owned()))
}

fn lookup_buffer(name: libc::c_int) -> io::Result<Vec<u8>> {
    // SAFETY: `name` is one of Darwin's documented sysconf selectors.
    let length = unsafe { libc::sysconf(name) };
    let length = usize::try_from(length)
        .map_err(|_| io::Error::other("sysconf returned no account lookup buffer size"))?;
    if length == 0 {
        return Err(io::Error::other(
            "sysconf returned an empty account lookup buffer size",
        ));
    }
    Ok(vec![0; length])
}
