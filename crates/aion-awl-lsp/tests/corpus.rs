//! Rev-2 corpus conformance gates for LSP diagnostics and formatting.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use aion_awl_lsp::{diagnostics, format_document};

type TestResult = Result<(), Box<dyn Error>>;

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../aion-awl/tests/fixtures/rev2")
}

fn families() -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut paths = fs::read_dir(corpus_root())?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| path.is_dir());
    paths.sort();
    Ok(paths)
}

fn awl_files(directory: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut paths = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| path.extension().is_some_and(|extension| extension == "awl"));
    paths.sort();
    Ok(paths)
}

struct Expectation {
    stage: String,
    substring: String,
    line: u32,
}

fn expectation(path: &Path) -> Result<Expectation, Box<dyn Error>> {
    let content = fs::read_to_string(path.with_extension("expected"))?;
    let mut lines = content.lines();
    let stage = lines.next().ok_or("sidecar missing stage")?.to_owned();
    if stage != "PARSE" && stage != "CHECK" {
        return Err(format!("{} has unknown stage {stage:?}", path.display()).into());
    }
    let substring = lines.next().ok_or("sidecar missing substring")?.to_owned();
    let line = lines.next().ok_or("sidecar missing line")?.trim().parse()?;
    Ok(Expectation {
        stage,
        substring,
        line,
    })
}

#[test]
fn lsp_g1_rev2_corpus_diagnostics_conform() -> TestResult {
    let mut valid_count = 0_usize;
    let mut invalid_count = 0_usize;
    for family in families()? {
        for path in awl_files(&family.join("valid"))? {
            let source = fs::read_to_string(&path)?;
            let found = diagnostics(&source, path.parent());
            if !found.is_empty() {
                return Err(format!(
                    "{} must have zero diagnostics; got {found:#?}",
                    path.display()
                )
                .into());
            }
            valid_count += 1;
        }
        for path in awl_files(&family.join("invalid"))? {
            let source = fs::read_to_string(&path)?;
            let expected = expectation(&path)?;
            let found = diagnostics(&source, path.parent());
            let matched = found.iter().any(|diagnostic| {
                diagnostic.message.contains(&expected.substring)
                    && diagnostic.range.start.line + 1 == expected.line
            });
            if !matched {
                return Err(format!(
                    "{}: no {} diagnostic contains {:?} on line {}; got {found:#?}",
                    path.display(),
                    expected.stage,
                    expected.substring,
                    expected.line
                )
                .into());
            }
            invalid_count += 1;
        }
    }
    println!(
        "LSP-G1: {valid_count} valid fixtures produced zero diagnostics; \
         {invalid_count} invalid sidecars matched message and 1-based start line"
    );
    assert!(valid_count >= 50, "only {valid_count} valid fixtures ran");
    assert!(
        invalid_count >= 79,
        "only {invalid_count} invalid fixtures ran"
    );
    Ok(())
}

#[test]
fn lsp_g2_all_valid_fixtures_use_the_canonical_idempotent_printer() -> TestResult {
    let mut formatted_count = 0_usize;
    for family in families()? {
        for path in awl_files(&family.join("valid"))? {
            let source = fs::read_to_string(&path)?;
            let document = aion_awl::parse(&source).map_err(|error| error.message)?;
            let expected = aion_awl::print(&document);
            let actual = format_document(&source).ok_or("valid fixture did not format")?;
            assert_eq!(actual, expected, "formatter drift for {}", path.display());
            assert_eq!(
                format_document(&actual).as_deref(),
                Some(actual.as_str()),
                "second format changed {}",
                path.display()
            );
            formatted_count += 1;
        }
    }
    println!(
        "LSP-G2: {formatted_count} valid fixtures formatted byte-identically and idempotently"
    );
    assert!(formatted_count >= 50, "only {formatted_count} fixtures ran");
    Ok(())
}
