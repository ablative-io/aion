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

/// Read + parse a fixture, then lower it. A read or parse failure of a
/// `valid/` fixture is a hard test error (returned as the outer `Err`), never
/// a silently-counted "deferred" outcome; the inner `Result` is the lowering
/// outcome (`Ok` covered, `Err` a recorded refusal).
fn lower_fixture(path: &Path) -> Result<Result<MirModule, LowerError>, Box<dyn std::error::Error>> {
    let source = fs::read_to_string(path)?;
    let document = crate::parse(&source)
        .map_err(|error| format!("valid fixture {} no longer parses: {error}", path.display()))?;
    Ok(lower(&document, path.parent()))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The exact set of `valid/` fixtures this BC-2 increment lowers, pinned so a
/// regression from covered → refused (or a newly-covered fixture) fails the
/// ratchet instead of being silently absorbed by the deferred bucket. Paths
/// are relative to `tests/fixtures/rev2`, without the `.awl` extension.
const COVERED: &[&str] = &[
    "dag-fork/valid/after_single",
    "dag-fork/valid/fall_through_chain",
    "declarations/valid/worker_retry_backoff",
    "declarations/valid/worker_single_action",
    "declarations/valid/workers_multiple",
    "flagship/valid/awl_hello",
    "header-types/valid/doc_comments",
    "header-types/valid/enum",
    "header-types/valid/line_width",
    "header-types/valid/minimal",
    "header-types/valid/noncanonical_commas",
    "schema-doors/valid/inline_verbatim_constraints",
    "step-bodies/valid/calls_and_side_effects",
    "step-bodies/valid/pipe_chain_stages",
];

/// Compare `contents` against the on-disk golden. A MISSING golden is a hard
/// failure (a covered fixture must have a committed golden) unless
/// `AWL_BC2_BLESS` is set, in which case the golden is (re)written. Blessing
/// NEVER happens implicitly.
fn check_golden(path: &Path, contents: &str) -> Result<(), String> {
    if std::env::var("AWL_BC2_BLESS").is_ok() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        return fs::write(path, contents).map_err(|error| error.to_string());
    }
    let expected = fs::read_to_string(path).map_err(|error| {
        format!(
            "missing golden {} (a covered fixture must have a committed golden; \
             run with AWL_BC2_BLESS=1 to create): {error}",
            path.display()
        )
    })?;
    if expected == contents {
        Ok(())
    } else {
        Err(format!("golden mismatch at {}", path.display()))
    }
}

/// Every committed `.mir` golden must belong to a fixture in the covered set;
/// an orphan (a golden whose fixture was deleted, renamed, or regressed to
/// refused) is a ratchet failure, not a silently-stale file.
fn walk_mir(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_mir(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "mir") {
            out.push(path);
        }
    }
}

fn committed_mir_goldens(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut paths = Vec::new();
    walk_mir(root, &mut paths);
    for path in paths {
        if let Ok(relative) = path.strip_prefix(root) {
            found.push(relative.with_extension("").to_string_lossy().into_owned());
        }
    }
    found.sort();
    found
}

#[test]
fn lowers_cover_and_golden() -> Result<(), Box<dyn std::error::Error>> {
    let fixtures_root = manifest_dir().join("tests/fixtures/rev2");
    let golden_root = manifest_dir().join("tests/mir-goldens");
    let mut covered = Vec::new();
    for fixture in valid_fixtures() {
        let relative = fixture.strip_prefix(&fixtures_root)?;
        let stem = relative.with_extension("").to_string_lossy().into_owned();
        // A read/parse failure of a valid fixture is a hard error (outer `?`);
        // only a genuine lowering refusal buckets as deferred.
        match lower_fixture(&fixture)? {
            Ok(module) => {
                verify(&module)?;
                let mir = print_mir(&module);
                let sidecar = hex(&project_sidecar(&module));
                let mir_path = golden_root.join(relative).with_extension("mir");
                let sidecar_path = golden_root.join(relative).with_extension("gleam_types.hex");
                check_golden(&mir_path, &mir)?;
                check_golden(&sidecar_path, &sidecar)?;
                covered.push(stem);
            }
            Err(LowerError::Unsupported { .. } | LowerError::Planning { .. }) => {}
            Err(other) => return Err(Box::new(other)),
        }
    }
    covered.sort();

    // The covered set is pinned exactly: a fixture regressing covered → refused
    // (or a newly-covered fixture) fails here instead of being absorbed.
    let mut expected: Vec<String> = COVERED.iter().map(|s| (*s).to_owned()).collect();
    expected.sort();
    assert_eq!(
        covered, expected,
        "BC-2 covered set drifted from the pinned COVERED list"
    );

    // No orphaned goldens: every committed `.mir` must map to a covered fixture.
    if std::env::var("AWL_BC2_BLESS").is_err() {
        let goldens = committed_mir_goldens(&golden_root);
        for golden in &goldens {
            assert!(
                covered.contains(golden),
                "orphaned golden {golden}.mir has no covered fixture"
            );
        }
        assert_eq!(
            goldens.len(),
            covered.len(),
            "committed golden count does not match the covered set"
        );
    }
    Ok(())
}

#[test]
fn minimal_exercises_core_op_shapes() -> Result<(), Box<dyn std::error::Error>> {
    let path = manifest_dir().join("tests/fixtures/rev2/header-types/valid/minimal.awl");
    let module = lower_fixture(&path)??;
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
fn deferred_fixture_errors_cleanly() -> Result<(), Box<dyn std::error::Error>> {
    let path =
        manifest_dir().join("tests/fixtures/rev2/loop-outcomes/valid/guard_optional_wait.awl");
    // A deferred fixture must not panic; it parses, then errors cleanly.
    let result = lower_fixture(&path)?;
    assert!(matches!(
        result,
        Err(LowerError::Unsupported { .. } | LowerError::Planning { .. })
    ));
    Ok(())
}
