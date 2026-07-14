use std::path::{Path, PathBuf};

use super::{
    entry_module_source_path, is_confined, is_supported_logical_module, single_entry_module,
    validate_project_root,
};
use crate::error::ToolchainError;

fn temp_root(label: &str) -> Result<PathBuf, std::io::Error> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
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
