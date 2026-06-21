//! Per-submission isolated build workspace.
//!
//! A network-facing authoring submission must never write to, or build inside,
//! the operator-provisioned project template: two concurrent submissions would
//! race on the same entry-module source file, the same `build/` directory, and
//! the same `.aion` output, so an author could receive another author's
//! artifact (or a half-overwritten one). The configured
//! `[authoring].project_root` is therefore a **read-only template** at request
//! time; every submission gets its own throwaway working copy.
//!
//! [`Workspace::stage`] creates a fresh temporary directory as a **sibling** of
//! the template — under the template's own parent directory — and recursively
//! copies the template's *contents* directly into it, so the working-copy root
//! sits at exactly the **same directory depth** as the template. Same-depth
//! sibling placement is load-bearing: a Gleam project's `aion_flow` path
//! dependency (and every `source = "local"` entry in `manifest.toml`) is
//! recorded **relative** to the project root, so a relative path such as
//! `../../gleam/aion_flow` resolves identically from a same-depth sibling as
//! from the template itself. Copying into an arbitrary location (the system
//! temp dir, say) — or nesting the working copy one directory deeper than the
//! template — would break those relative path dependencies; rewriting them
//! would mean parsing and re-emitting two TOML formats faithfully on every
//! request. Same-depth sibling placement preserves them untouched.
//!
//! The temporary directory is owned by the [`Workspace`] and removed when it is
//! dropped — on the success path and on every error path alike (the build
//! artifacts, including the captured submission source, never outlive the
//! request). The template is never mutated.

use std::path::Path;

use crate::error::ToolchainError;

/// An isolated, throwaway working copy of an authoring project template.
///
/// Created by [`Workspace::stage`]; the working copy lives under a temporary
/// directory that is removed when the `Workspace` is dropped. The submitted
/// source is written into, and the build runs entirely within,
/// [`Workspace::root`] — never the template.
pub struct Workspace {
    /// The owned temporary directory; its removal on drop is the cleanup. The
    /// temp directory **is** the working-copy project root — the template's
    /// contents are copied directly into it so the root sits at the same
    /// directory depth as the template (a same-depth sibling), and relative
    /// path dependencies resolve identically.
    temp: tempfile::TempDir,
}

impl Workspace {
    /// Stages a fresh, isolated working copy of `template_root`.
    ///
    /// Creates a temporary directory as a sibling of `template_root` (under its
    /// parent) and recursively copies the template's contents directly into it,
    /// so the working-copy root sits at the same directory depth as the
    /// template. Same-depth sibling placement preserves the template's relative
    /// path dependencies (`aion_flow` and any other `source = "local"` Gleam
    /// dependency): a path such as `../../gleam/aion_flow` resolves identically
    /// from the working copy as from the template root.
    ///
    /// The template is read only — it is never written to or built in. The
    /// returned [`Workspace`] owns the temporary directory and removes it on
    /// drop.
    ///
    /// # Errors
    ///
    /// Returns [`ToolchainError::InvalidProject`] when `template_root` has no
    /// parent directory (a filesystem root cannot host a sibling), and
    /// [`ToolchainError::Io`] when the sibling temporary directory cannot be
    /// created (for example the template's parent directory is not writable) or
    /// the recursive copy fails.
    pub fn stage(template_root: &Path) -> Result<Self, ToolchainError> {
        let parent = template_root
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| ToolchainError::InvalidProject {
                message: format!(
                    "authoring project root `{}` has no parent directory to host an isolated build workspace; the template must be provisioned inside a writable parent directory",
                    template_root.display()
                ),
            })?;

        let temp = tempfile::Builder::new()
            .prefix("aion-authoring-submission-")
            .tempdir_in(parent)
            .map_err(|source| ToolchainError::Io {
                path: parent.to_path_buf(),
                source,
            })?;

        // Copy the template's contents directly into the temp directory so the
        // working-copy root IS the temp directory — a same-depth sibling of the
        // template, never a directory deeper. This keeps relative path
        // dependencies (`../../gleam/aion_flow`) resolving identically.
        copy_tree(template_root, temp.path())?;

        Ok(Self { temp })
    }

    /// The isolated working-copy project root: where the submitted source is
    /// written and the build runs. It is the temporary directory itself — a
    /// same-depth sibling of the template.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.temp.path()
    }
}

impl std::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspace")
            .field("root", &self.temp.path())
            .finish()
    }
}

/// Recursively copies the directory tree at `from` into `to`, creating `to` and
/// every intermediate directory.
///
/// The template is operator-provisioned local content, not untrusted input, so
/// the copy mirrors files and directories faithfully. Every failure is a
/// path-carrying [`ToolchainError::Io`] — nothing is skipped silently.
fn copy_tree(from: &Path, to: &Path) -> Result<(), ToolchainError> {
    std::fs::create_dir_all(to).map_err(|source| ToolchainError::Io {
        path: to.to_path_buf(),
        source,
    })?;

    let entries = std::fs::read_dir(from).map_err(|source| ToolchainError::Io {
        path: from.to_path_buf(),
        source,
    })?;

    for entry in entries {
        let entry = entry.map_err(|source| ToolchainError::Io {
            path: from.to_path_buf(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| ToolchainError::Io {
            path: entry.path(),
            source,
        })?;
        let source_path = entry.path();
        let target_path = to.join(entry.file_name());

        if file_type.is_dir() {
            copy_tree(&source_path, &target_path)?;
        } else {
            std::fs::copy(&source_path, &target_path).map_err(|source| ToolchainError::Io {
                path: source_path.clone(),
                source,
            })?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::Workspace;
    use crate::error::ToolchainError;

    /// Builds a minimal template tree (gleam.toml, workflow.toml, nested src/,
    /// schemas/) under a fresh temp dir and returns the temp dir plus the
    /// template root inside it. The template root has a real parent so a
    /// sibling can be staged.
    fn template() -> Result<(tempfile::TempDir, std::path::PathBuf), Box<dyn std::error::Error>> {
        let parent = tempfile::Builder::new()
            .prefix("aion-toolchain-workspace-template-")
            .tempdir()?;
        let root = parent.path().join("project");
        std::fs::create_dir_all(root.join("src/nested"))?;
        std::fs::create_dir_all(root.join("schemas"))?;
        std::fs::write(root.join("gleam.toml"), b"name = \"demo\"\n")?;
        std::fs::write(root.join("workflow.toml"), b"[[workflow]]\n")?;
        std::fs::write(root.join("src/demo.gleam"), b"pub fn run() { Nil }\n")?;
        std::fs::write(root.join("src/nested/helper.gleam"), b"pub const x = 1\n")?;
        std::fs::write(root.join("schemas/input.json"), b"{}\n")?;
        Ok((parent, root))
    }

    #[test]
    fn stage_copies_the_full_tree_into_an_isolated_root() -> Result<(), Box<dyn std::error::Error>>
    {
        let (_parent, template_root) = template()?;
        let workspace = Workspace::stage(&template_root)?;

        let root = workspace.root();
        assert_ne!(
            root, template_root,
            "the workspace root is not the template"
        );
        assert!(root.join("gleam.toml").is_file());
        assert!(root.join("workflow.toml").is_file());
        assert!(root.join("src/demo.gleam").is_file());
        assert!(
            root.join("src/nested/helper.gleam").is_file(),
            "nested src modules are copied"
        );
        assert!(root.join("schemas/input.json").is_file());
        assert_eq!(
            std::fs::read(root.join("src/demo.gleam"))?,
            std::fs::read(template_root.join("src/demo.gleam"))?,
            "copied bytes match the template"
        );
        Ok(())
    }

    #[test]
    fn stage_places_the_workspace_as_a_same_depth_sibling_of_the_template()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_parent, template_root) = template()?;
        let template_parent = template_root.parent().ok_or("template has a parent")?;
        let workspace = Workspace::stage(&template_root)?;

        // The working-copy root IS the temp dir — a direct child of the
        // template's parent, at the SAME directory depth as the template. A
        // relative path dependency such as `../../gleam/aion_flow` therefore
        // resolves identically from the working copy as from the template.
        assert_eq!(
            workspace.root().parent(),
            Some(template_parent),
            "the workspace root is a same-depth sibling of the template under the same parent"
        );
        assert_eq!(
            workspace.root().components().count(),
            template_root.components().count(),
            "the workspace root sits at the same directory depth as the template"
        );
        Ok(())
    }

    #[test]
    fn dropping_the_workspace_removes_the_temp_dir_and_leaves_the_template()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_parent, template_root) = template()?;
        let workspace = Workspace::stage(&template_root)?;
        // The working-copy root IS the temp dir, so removing it on drop removes
        // the root itself.
        let staged_root = workspace.root().to_path_buf();
        assert!(staged_root.join("gleam.toml").is_file());

        // Mutate the working copy to prove the template is untouched.
        std::fs::write(staged_root.join("src/demo.gleam"), b"// overwritten\n")?;

        drop(workspace);

        assert!(
            !staged_root.exists(),
            "the workspace temp dir (the working-copy root) is removed on drop"
        );
        assert!(
            template_root.join("gleam.toml").is_file(),
            "the template is left intact"
        );
        assert_eq!(
            std::fs::read(template_root.join("src/demo.gleam"))?,
            b"pub fn run() { Nil }\n",
            "the template source is never mutated by a submission"
        );
        Ok(())
    }

    #[test]
    fn two_submissions_stage_into_distinct_isolated_roots() -> Result<(), Box<dyn std::error::Error>>
    {
        let (_parent, template_root) = template()?;
        let first = Workspace::stage(&template_root)?;
        let second = Workspace::stage(&template_root)?;

        assert_ne!(
            first.root(),
            second.root(),
            "concurrent submissions never share a working-copy root"
        );

        std::fs::write(first.root().join("src/demo.gleam"), b"// first\n")?;
        std::fs::write(second.root().join("src/demo.gleam"), b"// second\n")?;
        assert_eq!(
            std::fs::read(first.root().join("src/demo.gleam"))?,
            b"// first\n",
            "the first workspace is unaffected by writes to the second"
        );
        Ok(())
    }

    #[test]
    fn stage_rejects_a_template_without_a_parent_directory() {
        // The filesystem root has no parent to host a sibling.
        let result = Workspace::stage(Path::new("/"));
        assert!(
            matches!(result, Err(ToolchainError::InvalidProject { .. })),
            "a parentless template root is a typed InvalidProject, never a panic"
        );
    }
}
