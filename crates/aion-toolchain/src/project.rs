//! Project-root helpers for the authoring toolchain.
//!
//! The toolchain operates on a project root laid out exactly as `aion new`
//! and the examples produce one: a `gleam.toml`, a `workflow.toml`, a `src/`
//! tree, and `schemas/`. These helpers validate that the root is a usable
//! Gleam workflow project and resolve the on-disk path of the entry module's
//! source file, confining every write inside the project's `src/` directory
//! so a network-facing submission can never escape the root.

use std::path::{Component, Path, PathBuf};

use crate::error::ToolchainError;

/// File name of the Gleam project manifest.
const GLEAM_CONFIG_FILE: &str = "gleam.toml";

/// File name of the workflow packaging descriptor.
const WORKFLOW_CONFIG_FILE: &str = "workflow.toml";

/// The minimal `workflow.toml` shape this crate reads to derive the entry
/// module. Packaging proper re-parses the full descriptor through
/// `aion-package`; here we only need the declared entry modules, so unknown
/// keys are tolerated.
#[derive(serde::Deserialize)]
struct EntryConfig {
    #[serde(default)]
    workflow: Vec<EntryWorkflow>,
}

#[derive(serde::Deserialize)]
struct EntryWorkflow {
    entry_module: String,
}

#[derive(serde::Deserialize)]
struct GleamProjectConfig {
    name: String,
}

/// Validates that `root` is a usable Gleam workflow project: it must contain
/// both a `gleam.toml` and a `workflow.toml`.
///
/// # Errors
///
/// Returns [`ToolchainError::InvalidProject`] when either manifest is absent.
pub fn validate_project_root(root: &Path) -> Result<(), ToolchainError> {
    if !root.join(GLEAM_CONFIG_FILE).is_file() {
        return Err(ToolchainError::InvalidProject {
            message: format!(
                "{} not found under the authoring project root `{}`; the root must be a built Gleam project",
                GLEAM_CONFIG_FILE,
                root.display()
            ),
        });
    }
    if !root.join(WORKFLOW_CONFIG_FILE).is_file() {
        return Err(ToolchainError::InvalidProject {
            message: format!(
                "{} not found under the authoring project root `{}`; the root must declare its workflow packaging descriptor",
                WORKFLOW_CONFIG_FILE,
                root.display()
            ),
        });
    }
    Ok(())
}

/// Derives the single entry module declared by `<root>/workflow.toml`.
///
/// Submitting source is only meaningful for a single-workflow project: the
/// submitted Gleam is written to that one entry module's source file. A
/// project declaring zero or many workflows is rejected rather than guessed.
///
/// # Errors
///
/// Returns [`ToolchainError::Io`] when the descriptor cannot be read,
/// [`ToolchainError::InvalidProject`] when it cannot be parsed or does not
/// declare exactly one workflow.
pub fn single_entry_module(root: &Path) -> Result<String, ToolchainError> {
    let descriptor = root.join(WORKFLOW_CONFIG_FILE);
    let text = std::fs::read_to_string(&descriptor).map_err(|source| ToolchainError::Io {
        path: descriptor.clone(),
        source,
    })?;
    let config: EntryConfig =
        toml::from_str(&text).map_err(|source| ToolchainError::InvalidProject {
            message: format!("failed to parse {}: {source}", descriptor.display()),
        })?;
    match config.workflow.as_slice() {
        [single] => Ok(single.entry_module.clone()),
        [] => Err(ToolchainError::InvalidProject {
            message: format!(
                "{} declares no [[workflow]] entry; source submission requires exactly one",
                descriptor.display()
            ),
        }),
        many => Err(ToolchainError::InvalidProject {
            message: format!(
                "{} declares {} [[workflow]] entries; source submission requires exactly one entry module to write the submitted source into",
                descriptor.display(),
                many.len()
            ),
        }),
    }
}

/// Resolves the on-disk source path for `entry_module` under `<root>/src`,
/// confining it to that directory.
///
/// Gleam's internal nested-module separator `@` maps to a path separator under
/// `src/` (`demo@nested` -> `src/demo/nested.gleam`); source-style `/` separators
/// are also accepted. Every component must match `[a-z][a-z0-9_]*`, the exact
/// module grammar this toolchain supports, before any path is built.
///
/// # Errors
///
/// Returns [`ToolchainError::InvalidProject`] when the module name is not a
/// supported Gleam logical name or the resolved path escapes `<root>/src`.
pub fn entry_module_source_path(
    root: &Path,
    entry_module: &str,
) -> Result<PathBuf, ToolchainError> {
    if !is_supported_logical_module(entry_module) {
        return Err(ToolchainError::InvalidProject {
            message: format!(
                "entry module `{entry_module}` is not a supported Gleam logical module name (each `/` or `@` separated component must match `[a-z][a-z0-9_]*`)"
            ),
        });
    }

    let src_root = root.join("src");
    let relative: PathBuf = entry_module.split('@').collect::<PathBuf>();
    let mut candidate = src_root.join(relative);
    candidate.set_extension("gleam");

    // Defence in depth: even though the module grammar rejects traversal,
    // confirm lexically that the resolved path stays under
    // `<root>/src` before it is ever handed to the filesystem.
    if !is_confined(&src_root, &candidate) {
        return Err(ToolchainError::InvalidProject {
            message: format!(
                "entry module `{entry_module}` resolves outside the project src directory `{}`",
                src_root.display()
            ),
        });
    }
    Ok(candidate)
}

/// Writes `source` to the entry module's source file, creating any parent
/// module directories first.
///
/// # Errors
///
/// Returns [`ToolchainError::Io`] when a parent directory cannot be created or
/// the file cannot be written.
pub fn write_entry_source(path: &Path, source: &str) -> Result<(), ToolchainError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|io| ToolchainError::Io {
            path: parent.to_path_buf(),
            source: io,
        })?;
    }
    std::fs::write(path, source.as_bytes()).map_err(|io| ToolchainError::Io {
        path: path.to_path_buf(),
        source: io,
    })
}

/// Replaces the staged descriptor's sole entry module while preserving all
/// other workflow packaging policy from the operator's template.
///
/// This is deliberately limited to a staged workspace: callers retain the
/// configured template as read-only, while a document-aware submission can
/// compile and package under its own logical module name.
pub(crate) fn retarget_single_entry_module(
    root: &Path,
    entry_module: &str,
) -> Result<(), ToolchainError> {
    drop(entry_module_source_path(root, entry_module)?);
    let descriptor = root.join(WORKFLOW_CONFIG_FILE);
    let text = std::fs::read_to_string(&descriptor).map_err(|source| ToolchainError::Io {
        path: descriptor.clone(),
        source,
    })?;
    let mut config: toml::Value =
        toml::from_str(&text).map_err(|source| ToolchainError::InvalidProject {
            message: format!("failed to parse {}: {source}", descriptor.display()),
        })?;
    let workflows = config
        .get_mut("workflow")
        .and_then(toml::Value::as_array_mut)
        .ok_or_else(|| ToolchainError::InvalidProject {
            message: format!(
                "{} declares no [[workflow]] entry; source submission requires exactly one",
                descriptor.display()
            ),
        })?;
    let workflow = match workflows.as_mut_slice() {
        [single] => single,
        many => {
            return Err(ToolchainError::InvalidProject {
                message: format!(
                    "{} declares {} [[workflow]] entries; source submission requires exactly one entry module to write the submitted source into",
                    descriptor.display(),
                    many.len()
                ),
            });
        }
    };
    let table = workflow
        .as_table_mut()
        .ok_or_else(|| ToolchainError::InvalidProject {
            message: format!(
                "{} contains a non-table [[workflow]] entry",
                descriptor.display()
            ),
        })?;
    table.insert(
        "entry_module".to_owned(),
        toml::Value::String(entry_module.to_owned()),
    );
    let adjusted = toml::to_string(&config).map_err(|source| ToolchainError::InvalidProject {
        message: format!("failed to serialize {}: {source}", descriptor.display()),
    })?;
    std::fs::write(&descriptor, adjusted).map_err(|source| ToolchainError::Io {
        path: descriptor,
        source,
    })
}

/// Removes the frozen entry source from a staged workspace after its descriptor
/// has been retargeted. A missing source is valid because template validation
/// requires the manifest, not a pre-existing placeholder module.
pub(crate) fn remove_entry_source(path: &Path) -> Result<(), ToolchainError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ToolchainError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Removes the root package's compiler output copied from a prebuilt template.
///
/// Gleam's incremental build does not prune a `.beam` after its source is
/// removed. Explicit-entry submissions replace the frozen entry source, so the
/// staged root package must rebuild from empty output. Dependency outputs remain
/// available: they cannot contain the replaced first-party module and retaining
/// them avoids an unnecessary network resolution during an authoring deploy.
pub(crate) fn remove_staged_root_build(root: &Path) -> Result<(), ToolchainError> {
    let package = root_package_name(root)?;
    let build = root.join("build").join("dev").join("erlang").join(package);
    match std::fs::remove_dir_all(&build) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ToolchainError::Io {
            path: build,
            source,
        }),
    }
}

/// Removes Gleam's generated root-package application bootstrap after a clean
/// explicit-entry build.
///
/// `<package>@@main.beam` belongs to the frozen `gleam.toml` package shell, not
/// to the emitted workflow or a runtime dependency. Excluding it prevents the
/// template package name from entering document version identity. Generic
/// project compilation retains it; this ruling is specific to document-owned
/// packages assembled by `compile_source_for_entry`.
pub(crate) fn remove_generated_root_main_beam(root: &Path) -> Result<(), ToolchainError> {
    let package = root_package_name(root)?;
    let artifact = root
        .join("build")
        .join("dev")
        .join("erlang")
        .join(&package)
        .join("ebin")
        .join(format!("{package}@@main.beam"));
    match std::fs::remove_file(&artifact) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ToolchainError::Io {
            path: artifact,
            source,
        }),
    }
}

fn root_package_name(root: &Path) -> Result<String, ToolchainError> {
    let descriptor = root.join(GLEAM_CONFIG_FILE);
    let text = std::fs::read_to_string(&descriptor).map_err(|source| ToolchainError::Io {
        path: descriptor.clone(),
        source,
    })?;
    let config: GleamProjectConfig =
        toml::from_str(&text).map_err(|source| ToolchainError::InvalidProject {
            message: format!("failed to parse {}: {source}", descriptor.display()),
        })?;
    if !is_gleam_name_component(&config.name) {
        return Err(ToolchainError::InvalidProject {
            message: format!(
                "Gleam package name `{}` must match `[a-z][a-z0-9_]*`",
                config.name
            ),
        });
    }
    Ok(config.name)
}

/// Whether `candidate`, folded lexically, stays inside `base`.
///
/// Folds `.` and `..` components without touching the filesystem so the check
/// holds even before the target file exists.
fn is_confined(base: &Path, candidate: &Path) -> bool {
    let mut depth: i64 = 0;
    let Ok(relative) = candidate.strip_prefix(base) else {
        return false;
    };
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(_) => depth += 1,
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            // An absolute or prefix component inside the relative remainder
            // means the path was not actually under `base`.
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    depth >= 0
}

/// Whether a logical module uses the exact Gleam grammar supported here.
fn is_supported_logical_module(logical_name: &str) -> bool {
    logical_name.split(['/', '@']).all(is_gleam_name_component)
}

fn is_gleam_name_component(component: &str) -> bool {
    let mut bytes = component.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        entry_module_source_path, is_confined, is_supported_logical_module, single_entry_module,
        validate_project_root,
    };
    use crate::error::ToolchainError;

    fn temp_root(label: &str) -> Result<PathBuf, std::io::Error> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!("aion-toolchain-{label}-{nanos}"));
        std::fs::create_dir_all(&root)?;
        Ok(root)
    }

    #[test]
    fn supported_logical_modules_enforce_gleam_grammar_and_nesting() {
        for valid in ["hello_world", "demo2", "demo@nested", "demo/nested"] {
            assert!(is_supported_logical_module(valid), "must accept `{valid}`");
        }
        for invalid in [
            "",
            "Demo",
            "bad-name",
            "bad name",
            "../escape",
            "demo@..",
            "/abs",
            "demo$bad",
            "demo\\bad",
            "demo//nested",
            "demo/Upper",
        ] {
            assert!(
                !is_supported_logical_module(invalid),
                "must reject `{invalid}`"
            );
        }
    }

    #[test]
    fn entry_module_path_maps_nested_modules_under_src() -> Result<(), Box<dyn std::error::Error>> {
        let root = Path::new("/work");
        let flat = entry_module_source_path(root, "hello_world")?;
        assert_eq!(flat, PathBuf::from("/work/src/hello_world.gleam"));
        let nested = entry_module_source_path(root, "demo@nested")?;
        assert_eq!(nested, PathBuf::from("/work/src/demo/nested.gleam"));
        let source_style = entry_module_source_path(root, "demo/other")?;
        assert_eq!(source_style, PathBuf::from("/work/src/demo/other.gleam"));
        Ok(())
    }

    #[test]
    fn entry_module_path_rejects_traversal() {
        let root = Path::new("/work");
        let result = entry_module_source_path(root, "../../etc/passwd");
        assert!(matches!(result, Err(ToolchainError::InvalidProject { .. })));
    }

    #[test]
    fn confinement_folds_dotdot_lexically() {
        let base = Path::new("/work/src");
        assert!(is_confined(base, Path::new("/work/src/demo.gleam")));
        assert!(is_confined(base, Path::new("/work/src/demo/nested.gleam")));
        assert!(!is_confined(base, Path::new("/work/other.gleam")));
        assert!(!is_confined(base, Path::new("/work/src/../secret.gleam")));
    }

    #[test]
    fn validate_project_root_requires_both_manifests() -> Result<(), Box<dyn std::error::Error>> {
        let root = temp_root("validate")?;
        let cleanup = || {
            let _ = std::fs::remove_dir_all(&root);
        };

        let missing_gleam = validate_project_root(&root);
        assert!(matches!(
            missing_gleam,
            Err(ToolchainError::InvalidProject { .. })
        ));

        std::fs::write(root.join("gleam.toml"), b"name = \"demo\"\n")?;
        let missing_workflow = validate_project_root(&root);
        assert!(matches!(
            missing_workflow,
            Err(ToolchainError::InvalidProject { .. })
        ));

        std::fs::write(root.join("workflow.toml"), b"[[workflow]]\n")?;
        let ok = validate_project_root(&root);
        cleanup();
        ok?;
        Ok(())
    }

    #[test]
    fn single_entry_module_reads_the_descriptor() -> Result<(), Box<dyn std::error::Error>> {
        let root = temp_root("single-entry")?;
        std::fs::write(
            root.join("workflow.toml"),
            b"[[workflow]]\nentry_module = \"hello_world\"\nentry_function = \"run\"\ntimeout_seconds = 30\ninput_schema = \"schemas/input.json\"\noutput_schema = \"schemas/output.json\"\nactivities = []\n",
        )?;
        let entry = single_entry_module(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(entry?, "hello_world");
        Ok(())
    }

    #[test]
    fn many_entry_modules_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let root = temp_root("many-entry")?;
        std::fs::write(
            root.join("workflow.toml"),
            b"[[workflow]]\nentry_module = \"a\"\n\n[[workflow]]\nentry_module = \"b\"\n",
        )?;
        let entry = single_entry_module(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(matches!(entry, Err(ToolchainError::InvalidProject { .. })));
        Ok(())
    }

    #[test]
    fn zero_entry_modules_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let root = temp_root("zero-entry")?;
        std::fs::write(root.join("workflow.toml"), b"# no workflows declared\n")?;
        let entry = single_entry_module(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(matches!(entry, Err(ToolchainError::InvalidProject { .. })));
        Ok(())
    }
}
