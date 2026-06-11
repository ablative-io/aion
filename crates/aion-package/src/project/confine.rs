//! Lexical root-confinement for `workflow.toml`-declared paths.

use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use super::error::PackagingError;

/// Resolves a `workflow.toml`-declared path against the project root,
/// confining it to the root.
///
/// Purely lexical — no filesystem access: `.` components are dropped and
/// each `..` folds away the most recent kept component. The declared value
/// is rejected with [`PackagingError::PathEscapesRoot`] when it is absolute
/// (including drive/UNC prefixes) or when a `..` would climb above the root.
/// The returned path is `root` extended by the normalized components, so
/// textually different spellings of the same file (`out.aion` vs
/// `sub/../out.aion`) resolve identically.
///
/// This confinement applies only to descriptor-sourced paths. The
/// programmatic `PackageOptions::output_override` is the caller's own path
/// and is intentionally exempt.
pub(crate) fn resolve_confined(
    root: &Path,
    field: String,
    declared: &str,
) -> Result<PathBuf, PackagingError> {
    let escape = |field: String| PackagingError::PathEscapesRoot {
        field,
        path: PathBuf::from(declared),
    };

    let mut kept: Vec<&OsStr> = Vec::new();
    for component in Path::new(declared).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => kept.push(part),
            Component::ParentDir => {
                if kept.pop().is_none() {
                    return Err(escape(field));
                }
            }
            Component::RootDir | Component::Prefix(_) => return Err(escape(field)),
        }
    }

    let mut resolved = root.to_path_buf();
    resolved.extend(kept);
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::resolve_confined;
    use crate::project::error::PackagingError;

    const ROOT: &str = "/project";

    fn resolve(declared: &str) -> Result<PathBuf, PackagingError> {
        resolve_confined(Path::new(ROOT), "workflow[0].output".to_owned(), declared)
    }

    #[test]
    fn relative_paths_resolve_under_root() -> Result<(), PackagingError> {
        assert_eq!(resolve("demo.aion")?, Path::new("/project/demo.aion"));
        assert_eq!(
            resolve("dist/demo.aion")?,
            Path::new("/project/dist/demo.aion")
        );
        Ok(())
    }

    #[test]
    fn dot_and_inside_root_dotdot_components_fold_away() -> Result<(), PackagingError> {
        assert_eq!(resolve("./demo.aion")?, Path::new("/project/demo.aion"));
        assert_eq!(
            resolve("sub/../demo.aion")?,
            Path::new("/project/demo.aion")
        );
        assert_eq!(
            resolve("a/./b/../../c/demo.aion")?,
            Path::new("/project/c/demo.aion")
        );
        Ok(())
    }

    #[test]
    fn absolute_paths_are_rejected_with_field_and_declared_path() {
        let result = resolve("/tmp/outside.aion");

        assert!(matches!(
            result,
            Err(PackagingError::PathEscapesRoot { field, path })
                if field == "workflow[0].output" && path == Path::new("/tmp/outside.aion")
        ));
    }

    #[test]
    fn dotdot_climbing_above_root_is_rejected() {
        for declared in ["../escape.aion", "sub/../../escape.aion", ".."] {
            let result = resolve(declared);

            assert!(
                matches!(
                    result,
                    Err(PackagingError::PathEscapesRoot { ref path, .. })
                        if path == Path::new(declared)
                ),
                "`{declared}` was not rejected: {result:?}"
            );
        }
    }
}
