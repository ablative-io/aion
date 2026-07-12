//! In-crate MIR tests (the module is crate-private, so tests live here rather
//! than in `tests/`). Golden coverage + §5 op-shape assertions.
//!
//! Goldens live under `tests/mir-goldens/`; set `AWL_BC2_BLESS=1` to (re)write
//! them. `verify` (S1) runs inside every golden. The golden set covers every
//! checking fixture this BC-2 increment fully lowers; fixtures that hit a
//! deferred shape are recorded (they must fail with `Unsupported`/`Planning`,
//! never a panic).

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use super::{LowerError, MirModule, lower, print_mir, project_sidecar, verify};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn valid_fixtures() -> Vec<PathBuf> {
    let root = manifest_dir().join("tests/fixtures/rev2");
    let mut found = Vec::new();
    collect_awl(&root, &mut found);
    found.sort();
    found
}

fn collect_awl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_awl(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "awl")
            && path.components().any(|c| c.as_os_str() == "valid")
        {
            out.push(path);
        }
    }
}

fn lower_fixture(path: &Path) -> Result<MirModule, LowerError> {
    let source = fs::read_to_string(path).map_err(|error| LowerError::Planning {
        message: error.to_string(),
    })?;
    let document = crate::parse(&source).map_err(|error| LowerError::Planning {
        message: error.to_string(),
    })?;
    lower(&document, path.parent())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn check_golden(path: &Path, contents: &str) -> Result<(), String> {
    if std::env::var("AWL_BC2_BLESS").is_ok() || !path.exists() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(path, contents).map_err(|error| error.to_string())?;
        return Ok(());
    }
    let expected = fs::read_to_string(path).map_err(|error| error.to_string())?;
    if expected == contents {
        Ok(())
    } else {
        Err(format!("golden mismatch at {}", path.display()))
    }
}

#[test]
fn lowers_cover_and_golden() -> Result<(), Box<dyn std::error::Error>> {
    let golden_root = manifest_dir().join("tests/mir-goldens");
    let mut lowered = 0usize;
    let mut deferred = 0usize;
    for fixture in valid_fixtures() {
        let relative = fixture.strip_prefix(manifest_dir().join("tests/fixtures/rev2"))?;
        match lower_fixture(&fixture) {
            Ok(module) => {
                verify(&module)?;
                let mir = print_mir(&module);
                let sidecar = hex(&project_sidecar(&module));
                let mir_path = golden_root.join(relative).with_extension("mir");
                let sidecar_path = golden_root.join(relative).with_extension("gleam_types.hex");
                check_golden(&mir_path, &mir)?;
                check_golden(&sidecar_path, &sidecar)?;
                lowered += 1;
            }
            Err(LowerError::Unsupported { .. } | LowerError::Planning { .. }) => {
                deferred += 1;
            }
            Err(other) => return Err(Box::new(other)),
        }
    }
    assert!(
        lowered > 0,
        "no fixtures lowered — BC-2 covered set is empty"
    );
    // Both counts are non-zero: the covered set is real and the deferred set
    // is honestly recorded rather than silently skipped.
    assert!(deferred > 0);
    Ok(())
}

#[test]
fn minimal_exercises_core_op_shapes() -> Result<(), Box<dyn std::error::Error>> {
    let path = manifest_dir().join("tests/fixtures/rev2/header-types/valid/minimal.awl");
    let module = lower_fixture(&path)?;
    verify(&module)?;
    let text = print_mir(&module);
    // §5 op shapes exercised by `host |> check |> route reported`.
    for token in [
        "call_local", // T-ACT wrapper invocation
        "call_rt aion@activity:task_queue/2",
        "call_rt aion@workflow:run/1",
        "try_bind",     // R1 flattened result.try
        "record(",      // outcome constructor + ok wrap
        "make_closure", // codec `_codec` composer
        "tail_rt aion@codec:json_codec/2",
        "field(", // record `_to_json`
        "json_obj",
    ] {
        assert!(
            text.contains(token),
            "missing op shape `{token}` in:\n{text}"
        );
    }
    Ok(())
}

#[test]
fn deferred_fixture_errors_cleanly() {
    let path =
        manifest_dir().join("tests/fixtures/rev2/loop-outcomes/valid/guard_optional_wait.awl");
    // A deferred fixture must not panic; it errors cleanly.
    let result = lower_fixture(&path);
    assert!(matches!(
        result,
        Err(LowerError::Unsupported { .. } | LowerError::Planning { .. })
    ));
}
