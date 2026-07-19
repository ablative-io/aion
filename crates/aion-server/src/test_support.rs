//! Test-only filesystem helpers.

use tempfile::TempDir;

/// Creates a temporary directory whose mode is `0700` regardless of the
/// process umask.
///
/// `tempfile::tempdir` inherits the umask, so under a conventional `022`
/// a fresh directory is `0755` — which the server's private-root
/// validation rightly refuses. Tests that hand a temporary directory to
/// any sensitive-root surface must go through this helper so the suite
/// is hermetic under every umask.
/// Tightens an already-created directory to mode `0700` so a test-built
/// workspace root passes private-root validation under any umask.
pub(crate) fn make_private(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

pub(crate) fn private_tempdir() -> std::io::Result<TempDir> {
    let dir = tempfile::tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}
