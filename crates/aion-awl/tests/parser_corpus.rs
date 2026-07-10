//! The rev-2 golden corpus is the parser's objective gate: every valid
//! fixture parses; every invalid fixture whose sidecar stage is PARSE fails
//! with the expected diagnostic substring at the expected line; every
//! CHECK-staged fixture parses cleanly (its rejection is the checker
//! phase's duty, so the parser must let it through).

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use aion_awl::parse;

type TestResult = Result<(), Box<dyn Error>>;

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rev2")
}

fn awl_files(dir: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut files = Vec::new();
    if !dir.exists() {
        return Ok(files);
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "awl") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn families() -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(corpus_root())? {
        let path = entry?.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

struct Expectation {
    stage: String,
    substring: String,
    line: usize,
}

fn read_expectation(path: &Path) -> Result<Expectation, Box<dyn Error>> {
    let sidecar = path.with_extension("expected");
    let content = fs::read_to_string(&sidecar)?;
    let mut lines = content.lines();
    let stage = lines.next().ok_or("sidecar missing stage line")?.to_owned();
    let substring = lines
        .next()
        .ok_or("sidecar missing substring line")?
        .to_owned();
    let line = lines
        .next()
        .ok_or("sidecar missing line-number line")?
        .trim()
        .parse::<usize>()?;
    Ok(Expectation {
        stage,
        substring,
        line,
    })
}

#[test]
fn corpus_has_not_shrunk() -> TestResult {
    let mut valid = 0;
    let mut invalid = 0;
    for family in families()? {
        valid += awl_files(&family.join("valid"))?.len();
        invalid += awl_files(&family.join("invalid"))?.len();
    }
    assert!(valid >= 50, "valid corpus shrank: {valid} fixtures");
    assert!(invalid >= 108, "invalid corpus shrank: {invalid} fixtures");
    Ok(())
}

#[test]
fn every_valid_fixture_parses() -> TestResult {
    let mut checked = 0;
    for family in families()? {
        for path in awl_files(&family.join("valid"))? {
            let source = fs::read_to_string(&path)?;
            if let Err(error) = parse(&source) {
                return Err(format!(
                    "{} failed to parse: {} at line {}, column {}",
                    path.display(),
                    error.message,
                    error.span.line,
                    error.span.column
                )
                .into());
            }
            checked += 1;
        }
    }
    assert!(checked >= 50);
    Ok(())
}

#[test]
fn every_parse_staged_invalid_fixture_fails_as_expected() -> TestResult {
    let mut checked = 0;
    for family in families()? {
        for path in awl_files(&family.join("invalid"))? {
            let expectation = read_expectation(&path)?;
            if expectation.stage != "PARSE" {
                continue;
            }
            let source = fs::read_to_string(&path)?;
            let Err(error) = parse(&source) else {
                return Err(format!("{} parsed but must fail", path.display()).into());
            };
            if !error.message.contains(&expectation.substring) {
                return Err(format!(
                    "{}: diagnostic {:?} does not contain {:?}",
                    path.display(),
                    error.message,
                    expectation.substring
                )
                .into());
            }
            if error.span.line != expectation.line {
                return Err(format!(
                    "{}: diagnostic anchored at line {} (column {}), expected line {} — {:?}",
                    path.display(),
                    error.span.line,
                    error.span.column,
                    expectation.line,
                    error.message
                )
                .into());
            }
            checked += 1;
        }
    }
    assert!(checked >= 34, "only {checked} PARSE-staged fixtures ran");
    Ok(())
}

#[test]
fn every_check_staged_invalid_fixture_parses_cleanly() -> TestResult {
    let mut checked = 0;
    for family in families()? {
        for path in awl_files(&family.join("invalid"))? {
            let expectation = read_expectation(&path)?;
            if expectation.stage != "CHECK" {
                continue;
            }
            let source = fs::read_to_string(&path)?;
            if let Err(error) = parse(&source) {
                return Err(format!(
                    "{} is CHECK-staged but failed to parse: {} at line {}, column {}",
                    path.display(),
                    error.message,
                    error.span.line,
                    error.span.column
                )
                .into());
            }
            checked += 1;
        }
    }
    assert!(checked >= 74, "only {checked} CHECK-staged fixtures ran");
    Ok(())
}
