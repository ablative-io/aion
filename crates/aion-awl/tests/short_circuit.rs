//! Cross-backend regression for boolean guards with optional RHS access.

use std::error::Error;
use std::fs;
use std::path::PathBuf;

use aion_awl::{check_in, emit_in, parse};

#[test]
fn gleam_emitter_keeps_optional_rhs_inside_some_branch() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rev2/schema-doors/valid/short_circuit_optional.awl");
    let source = fs::read_to_string(&path)?;
    let document = parse(&source)?;
    let root = path.parent().ok_or("fixture has no parent")?;
    let errors = check_in(&document, root);
    assert!(errors.is_empty(), "fixture failed checking: {errors:#?}");
    let gleam = emit_in(&document, root)?;

    let expected = "case detail {\n  Some(detail) -> detail.approved\n  None -> True\n}";
    assert!(
        gleam.contains(expected),
        "optional RHS was not rendered under a Some-binding:\n{gleam}"
    );
    Ok(())
}
