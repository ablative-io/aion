use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use aion_awl::{check, parse};

const EXPECTED_FAILURES: &[&str] = &[
    // AWL1-004 (typed child contracts) will make these fixtures pass and remove this carve-out.
    "bounded_cycle.awl",
    "bounded_cycle.canonical.awl",
];

#[test]
fn committed_awl_fixtures_check_except_known_typed_child_contract_gap() -> Result<(), Box<dyn Error>>
{
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut fixtures = awl_fixtures(&fixture_dir)?;
    fixtures.sort();

    for path in fixtures {
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            return Err(format!("fixture path is not valid UTF-8: {}", path.display()).into());
        };
        let source = fs::read_to_string(&path)?;
        let document = parse(&source)?;
        let errors = check(&document);
        let expected_failure = EXPECTED_FAILURES.contains(&name);

        if expected_failure {
            if errors.is_empty() {
                return Err(
                    format!("expected known checker failure for {name}, but it passed").into(),
                );
            }
        } else if !errors.is_empty() {
            return Err(format!("fixture {name} failed checker: {errors:?}").into());
        }
    }

    Ok(())
}

fn awl_fixtures(fixture_dir: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut fixtures = Vec::new();
    for entry in fs::read_dir(fixture_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("awl") {
            fixtures.push(path);
        }
    }
    Ok(fixtures)
}
