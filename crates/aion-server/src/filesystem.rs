//! Descriptor-relative, no-follow filesystem operations for sensitive server roots.
//!
//! Every path component is opened from a held directory capability. Symlinks and
//! Windows reparse points are refused at component boundaries, and final files are
//! opened with no-follow semantics. A concurrent local actor may replace a name
//! after validation, but the operation remains relative to an already-open parent
//! descriptor: it can redirect a name within that held directory, never expand the
//! operation's authority beyond the configured root.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, DirBuilder, OpenOptions};
#[cfg(unix)]
use cap_std::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
#[cfg(all(unix, not(target_os = "macos")))]
use std::os::fd::AsRawFd as _;
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStringExt as _;

const PRIVATE_DIR_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

/// A held directory descriptor confining all subsequent operations beneath it.
pub(crate) struct ConfinedDir {
    dir: Dir,
}

impl ConfinedDir {
    /// Open an existing real directory without following any path component.
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let root = Self {
            dir: open_absolute(path, false)?,
        };
        root.require_private_mode(path)?;
        Ok(root)
    }

    /// Open or create a real directory, creating every missing component privately.
    pub(crate) fn open_or_create(path: &Path) -> io::Result<Self> {
        let root = Self {
            dir: open_absolute(path, true)?,
        };
        root.require_private_mode(path)?;
        Ok(root)
    }

    /// Read a UTF-8 file without following any component or final symlink.
    pub(crate) fn read_to_string(&self, relative: &Path) -> io::Result<String> {
        let mut file = self.open_file(relative, false)?;
        let mut value = String::new();
        file.read_to_string(&mut value)?;
        Ok(value)
    }

    /// Read a file without following any component or final symlink.
    pub(crate) fn read(&self, relative: &Path) -> io::Result<Vec<u8>> {
        let mut file = self.open_file(relative, false)?;
        let mut value = Vec::new();
        file.read_to_end(&mut value)?;
        Ok(value)
    }

    /// Create a new private file, refusing an existing file or link.
    pub(crate) fn create_new(&self, relative: &Path, bytes: &[u8]) -> io::Result<()> {
        let (parent, name) = self.open_parent(relative, true)?;
        let mut options = private_file_options();
        options.write(true).create_new(true);
        let mut file = parent.open_with(name, &options)?;
        if let Err(error) = write_and_sync(&mut file, bytes) {
            drop(file);
            let _ = parent.remove_file(name);
            return Err(error);
        }
        Ok(())
    }

    /// Atomically replace a private file using an unpredictable `create_new`
    /// temporary in the same held parent directory.
    pub(crate) fn atomic_write(&self, relative: &Path, bytes: &[u8]) -> io::Result<()> {
        let (parent, name) = self.open_parent(relative, true)?;
        match parent.symlink_metadata(name) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "refusing to replace a symbolic link",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let temp_name = OsString::from(format!(".aion-{}.tmp", uuid::Uuid::new_v4()));
        let mut options = private_file_options();
        options.write(true).create_new(true);
        let mut temp = parent.open_with(&temp_name, &options)?;
        if let Err(error) = write_and_sync(&mut temp, bytes) {
            drop(temp);
            let _ = parent.remove_file(&temp_name);
            return Err(error);
        }
        drop(temp);
        if let Err(error) = parent.rename(&temp_name, &parent, name) {
            let _ = parent.remove_file(&temp_name);
            return Err(error);
        }
        Ok(())
    }

    /// Remove a file relative to this capability, without traversing parents.
    pub(crate) fn remove_file(&self, relative: &Path) -> io::Result<()> {
        let (parent, name) = self.open_parent(relative, false)?;
        parent.remove_file(name)
    }

    /// Recursively list `.awl` files while refusing directory links.
    pub(crate) fn list_awl(&self) -> io::Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        visit_awl(&self.dir, Path::new(""), &mut paths)?;
        Ok(paths)
    }

    /// Eagerly create a descendant directory through this capability.
    pub(crate) fn create_dir_all(&self, relative: &Path) -> io::Result<()> {
        drop(self.open_dir(relative, true)?);
        Ok(())
    }

    /// Return the narrowest path bridge the platform offers from this held
    /// descriptor to a backend that accepts only `PathBuf`.
    ///
    /// Linux/Android can traverse descendants through `/proc/self/fd`, so the
    /// returned path remains descriptor-authoritative. macOS and other Unix
    /// targets expose a directory descriptor in `/dev/fd` but do not permit
    /// descendant traversal through that name; there we resolve the descriptor's
    /// current real path immediately before backend startup. Eagerly materializing
    /// every backend shard closes later lazy races, but a pathname-swap race remains
    /// between this resolution and completion of startup on those platforms until
    /// Haematite exposes a descriptor-relative constructor.
    #[cfg(unix)]
    pub(crate) fn backend_path(&self) -> io::Result<PathBuf> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            Ok(Path::new("/proc/self/fd").join(self.dir.as_raw_fd().to_string()))
        }
        #[cfg(target_os = "macos")]
        {
            let path = rustix::fs::getpath(&self.dir)?;
            Ok(PathBuf::from(OsString::from_vec(path.into_bytes())))
        }
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
        {
            std::fs::canonicalize(Path::new("/dev/fd").join(self.dir.as_raw_fd().to_string()))
        }
    }

    /// Return metadata for this held root directory.
    pub(crate) fn metadata(&self) -> io::Result<cap_std::fs::Metadata> {
        self.dir.dir_metadata()
    }

    /// Apply private modes to every existing descendant without following links.
    /// On non-Unix targets this performs no ACL mutation. Startup permits that
    /// limitation only for roots the operator explicitly configured and emits a
    /// warning that ACL privacy was not verified.
    pub(crate) fn harden_tree(&self) -> io::Result<()> {
        #[cfg(unix)]
        harden_dir(&self.dir)?;
        Ok(())
    }

    fn require_private_mode(&self, path: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            let mode = self.metadata()?.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "sensitive root `{}` has mode {mode:04o}; run `chmod 700 {}`",
                        path.display(),
                        path.display()
                    ),
                ));
            }
        }
        #[cfg(not(unix))]
        let _ = path;
        Ok(())
    }

    fn open_file(&self, relative: &Path, create_parents: bool) -> io::Result<cap_std::fs::File> {
        let (parent, name) = self.open_parent(relative, create_parents)?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        parent.open_with(name, &options)
    }

    fn open_parent<'a>(&self, relative: &'a Path, create: bool) -> io::Result<(Dir, &'a OsStr)> {
        validate_relative(relative)?;
        let name = relative.file_name().ok_or_else(invalid_relative)?;
        let parent = relative.parent().unwrap_or_else(|| Path::new(""));
        self.open_dir(parent, create).map(|dir| (dir, name))
    }

    fn open_dir(&self, relative: &Path, create: bool) -> io::Result<Dir> {
        validate_relative_or_empty(relative)?;
        let mut current = self.dir.try_clone()?;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(invalid_relative());
            };
            current = open_child_dir(&current, name, create)?;
        }
        Ok(current)
    }
}

fn open_absolute(path: &Path, create: bool) -> io::Result<Dir> {
    let absolute = std::path::absolute(path)?;
    let (anchor, names) = split_absolute(&absolute)?;
    let mut current = Dir::open_ambient_dir(&anchor, ambient_authority())?;
    for (index, name) in names.into_iter().enumerate() {
        match open_child_dir(&current, &name, create) {
            Ok(child) => current = child,
            Err(error) if index == 0 => {
                // macOS exposes root-owned compatibility aliases such as
                // `/var -> /private/var`. Following only a filesystem-root
                // entry preserves those platform paths; every user-controlled
                // component below it remains descriptor-relative and no-follow.
                let alias = anchor.join(&name);
                let metadata = std::fs::symlink_metadata(&alias)?;
                if !metadata.file_type().is_symlink() {
                    return Err(component_error(&name, &error));
                }
                let canonical = std::fs::canonicalize(alias)?;
                current = Dir::open_ambient_dir(canonical, ambient_authority())?;
            }
            Err(error) => return Err(component_error(&name, &error)),
        }
    }
    Ok(current)
}

fn split_absolute(path: &Path) -> io::Result<(PathBuf, Vec<OsString>)> {
    let mut anchor = PathBuf::new();
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => anchor.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if names.pop().is_none() {
                    return Err(invalid_relative());
                }
            }
            Component::Normal(name) => names.push(name.to_owned()),
        }
    }
    if anchor.as_os_str().is_empty() {
        return Err(invalid_relative());
    }
    Ok((anchor, names))
}

fn component_error(name: &OsStr, error: &io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!(
            "failed to open real directory component `{}`: {error}",
            name.to_string_lossy()
        ),
    )
}

fn open_child_dir(parent: &Dir, name: &OsStr, create: bool) -> io::Result<Dir> {
    match parent.open_dir_nofollow(name) {
        Ok(dir) => Ok(dir),
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            #[cfg(unix)]
            builder.mode(PRIVATE_DIR_MODE);
            match parent.create_dir_with(name, &builder) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            parent.open_dir_nofollow(name)
        }
        Err(error) => Err(error),
    }
}

fn private_file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.follow(FollowSymlinks::No);
    #[cfg(unix)]
    options.mode(PRIVATE_FILE_MODE);
    options
}

fn write_and_sync(file: &mut cap_std::fs::File, bytes: &[u8]) -> io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()
}

fn visit_awl(dir: &Dir, relative: &Path, paths: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in dir.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let child_relative = relative.join(&name);
        if file_type.is_dir() {
            let child = dir.open_dir_nofollow(&name)?;
            visit_awl(&child, &child_relative, paths)?;
        } else if file_type.is_file() && child_relative.extension() == Some(OsStr::new("awl")) {
            paths.push(child_relative);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn harden_dir(dir: &Dir) -> io::Result<()> {
    dir.set_permissions(
        Path::new("."),
        cap_std::fs::Permissions::from_mode(PRIVATE_DIR_MODE),
    )?;
    for entry in dir.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "sensitive state contains symbolic link `{}`",
                    name.to_string_lossy()
                ),
            ));
        }
        if file_type.is_dir() {
            let child = dir.open_dir_nofollow(&name)?;
            harden_dir(&child)?;
        } else if file_type.is_file() {
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            let file = dir.open_with(&name, &options)?;
            file.set_permissions(cap_std::fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
        }
    }
    Ok(())
}

fn validate_relative(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() {
        return Err(invalid_relative());
    }
    validate_relative_or_empty(path)
}

fn validate_relative_or_empty(path: &Path) -> io::Result<()> {
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid_relative());
    }
    Ok(())
}

fn invalid_relative() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "path must be relative and contain only normal components",
    )
}

/// Refuse an existing sensitive root that grants any Unix group/world access.
///
/// On non-Unix targets this function verifies only that an existing root is a
/// real directory. The configuration-resolution boundary separately refuses
/// default roots and warns for explicitly configured roots because ACL privacy
/// is not implemented or claimed here.
pub(crate) fn validate_private_root(path: &Path, label: &str) -> io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} `{}` is not a real directory", path.display()),
        ));
    }
    #[cfg(unix)]
    {
        let mode = std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o777;
        if mode & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{label} `{}` has mode {mode:04o}; run `chmod 700 {}` before starting Aion",
                    path.display(),
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn nested_sensitive_roots_and_files_ignore_a_permissive_umask()
    -> Result<(), Box<dyn std::error::Error>> {
        const PROBE: &str = "AION_PRIVATE_MODE_UMASK_PROBE";
        if let Some(path) = std::env::var_os(PROBE) {
            return assert_private_creation(Path::new(&path));
        }

        let sandbox = tempfile::tempdir()?;
        let executable = std::env::current_exe()?;
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(
                "umask 000; exec \"$1\" --exact \
                 filesystem::tests::nested_sensitive_roots_and_files_ignore_a_permissive_umask \
                 --nocapture",
            )
            .arg("aion-private-mode-probe")
            .arg(executable)
            .env(PROBE, sandbox.path())
            .status()?;
        assert!(status.success(), "private-mode umask probe failed");
        Ok(())
    }

    fn assert_private_creation(sandbox: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let home = sandbox.join("aion-home");
        let authoring = home.join("authoring");
        let root = ConfinedDir::open_or_create(&authoring)?;
        root.create_new(Path::new("private.txt"), b"secret")?;
        assert_eq!(
            std::fs::metadata(&home)?.permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&authoring)?.permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(authoring.join("private.txt"))?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        Ok(())
    }

    #[test]
    fn permissive_existing_root_fails_with_precise_remediation()
    -> Result<(), Box<dyn std::error::Error>> {
        let sandbox = tempfile::tempdir()?;
        let home = sandbox.path().join("aion-home");
        std::fs::create_dir(&home)?;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755))?;

        let error = validate_private_root(&home, "Aion home")
            .err()
            .ok_or("expected permissive home refusal")?;
        let message = error.to_string();
        assert!(message.contains("mode 0755"));
        assert!(message.contains("chmod 700"));
        assert!(ConfinedDir::open(&home).is_err());
        Ok(())
    }
}
