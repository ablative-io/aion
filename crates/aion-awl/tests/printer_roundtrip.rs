//! The parser and canonical printer are one property: over the entire
//! valid rev-2 corpus (flagship pair included), `print(parse(src)) == src`
//! byte-for-byte — except the deliberately non-canonical comma-tolerance
//! fixture, which must still normalize idempotently. Comments and doc
//! lines are lossless through the round trip.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use aion_awl::{parse, print};

type TestResult = Result<(), Box<dyn Error>>;

/// Valid fixtures that are deliberately NOT in canonical form; they parse
/// and normalize idempotently but are excluded from byte identity.
const NON_CANONICAL: &[&str] = &["noncanonical_commas.awl"];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rev2")
}

fn valid_fixtures() -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(corpus_root())? {
        let family = entry?.path();
        let valid = family.join("valid");
        if !valid.is_dir() {
            continue;
        }
        for entry in fs::read_dir(valid)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "awl") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn first_diff(left: &str, right: &str) -> String {
    for (index, (a, b)) in left.lines().zip(right.lines()).enumerate() {
        if a != b {
            return format!("line {}:\n  source : {a:?}\n  printed: {b:?}", index + 1);
        }
    }
    format!(
        "line counts differ: source {} vs printed {}",
        left.lines().count(),
        right.lines().count()
    )
}

#[test]
fn parse_print_is_identity_over_the_canonical_corpus() -> TestResult {
    let mut checked = 0;
    for path in valid_fixtures()? {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if NON_CANONICAL.contains(&name) {
            continue;
        }
        let source = fs::read_to_string(&path)?;
        let printed = print(
            &parse(&source)
                .map_err(|error| format!("{}: parse failed: {error}", path.display()))?,
        );
        if printed != source {
            return Err(format!(
                "{} did not round-trip byte-identically; first divergence at {}",
                path.display(),
                first_diff(&source, &printed)
            )
            .into());
        }
        checked += 1;
    }
    assert!(checked >= 49, "only {checked} fixtures round-tripped");
    Ok(())
}

#[test]
fn print_parse_print_is_idempotent_over_the_whole_corpus() -> TestResult {
    let mut checked = 0;
    for path in valid_fixtures()? {
        let source = fs::read_to_string(&path)?;
        let once = print(&parse(&source)?);
        let twice = print(&parse(&once).map_err(|error| {
            format!(
                "{}: canonical output failed to re-parse: {error}",
                path.display()
            )
        })?);
        if once != twice {
            return Err(format!(
                "{} print is not idempotent; first divergence at {}",
                path.display(),
                first_diff(&once, &twice)
            )
            .into());
        }
        checked += 1;
    }
    assert!(checked >= 50);
    Ok(())
}

#[test]
fn comma_tolerance_normalizes_to_canonical_form() -> TestResult {
    let path = corpus_root().join("header-types/valid/noncanonical_commas.awl");
    let source = fs::read_to_string(&path)?;
    let printed = print(&parse(&source)?);
    // The multi-line body without commas collapses to the canonical
    // single-line form; the single-line trailing comma is dropped.
    assert!(
        printed.contains("type Ledger { id: String, entries: [String] }"),
        "Ledger did not normalize:\n{printed}"
    );
    assert!(
        printed.contains("type Receipt { id: String, ledger: Ledger }"),
        "Receipt did not normalize:\n{printed}"
    );
    Ok(())
}

#[test]
fn trailing_comments_survive_and_normalize() -> TestResult {
    let source = concat!(
        "//! Trailing comments everywhere.\n",
        "workflow trailing // on the header\n",
        "  input name: String // on an input\n",
        "  outcome done: type Out, route success // on an outcome\n",
        "\n",
        "type Out { text: String } // on a type\n",
        "\n",
        "worker voice // on a worker\n",
        "  action shout(text: String) -> Out // on an action\n",
        "    node loud, timeout 5m // on a config line\n",
        "\n",
        "step run // on a step\n",
        "  shout(text: name) -> out // on a call\n",
        "  out |> route done // on a pipe\n",
    );
    let printed = print(&parse(source)?);
    for needle in [
        "workflow trailing // on the header",
        "input name: String // on an input",
        "route success // on an outcome",
        "type Out { text: String } // on a type",
        "worker voice // on a worker",
        "-> Out // on an action",
        "timeout 5m // on a config line",
        "step run // on a step",
        "-> out // on a call",
        "route done // on a pipe",
    ] {
        assert!(
            printed.contains(needle),
            "missing {needle:?} in:\n{printed}"
        );
    }
    assert_eq!(printed, print(&parse(&printed)?), "not idempotent");
    Ok(())
}

#[test]
fn stageless_bind_over_the_width_limit_stays_on_one_line_and_reparses() -> TestResult {
    // A stage-less bind has no `|>` to break before; wrapping it would put
    // `-> name` on a continuation line the parser cannot rejoin. It must
    // stay on one line at any width and the printed output must re-parse.
    let source = concat!(
        "//! Stage-less bind wrapping.\n",
        "workflow wrap\n",
        "  input name_bound_to_a_rather_long_identifier: Out\n",
        "  outcome finished: type Out, route success\n",
        "\n",
        "type Out { text: String }\n",
        "\n",
        "worker w\n",
        "  action noop(a: String) -> Out\n",
        "\n",
        "step run\n",
        "  name_bound_to_a_rather_long_identifier.field_one_is_long.field_two_is_longer",
        ".field_three_longest -> bound // keep me\n",
        "  bound |> route finished\n",
    );
    let printed = print(&parse(source)?);
    assert!(
        printed.contains(".field_three_longest -> bound // keep me\n"),
        "stage-less bind wrapped or lost its comment:\n{printed}"
    );
    let reparsed =
        parse(&printed).map_err(|error| format!("canonical output failed to re-parse: {error}"))?;
    assert_eq!(printed, print(&reparsed), "not idempotent");
    Ok(())
}

#[test]
fn wrapped_bind_chain_keeps_its_trailing_comment() -> TestResult {
    // A >100-column chain that ends in `-> name` breaks before each `|>`;
    // the `-> name` binding and the chain's trailing comment both live on
    // the last stage's line — the comment must never be dropped.
    let source = concat!(
        "//! Wrapped bind chain trailing comment.\n",
        "workflow wrap\n",
        "  input name: String\n",
        "  outcome finished: type Out, route success\n",
        "\n",
        "type Out { text: String }\n",
        "\n",
        "worker w\n",
        "  action shout(text: String) -> Out\n",
        "\n",
        "step run\n",
        "  name |> shout |> .text |> shout |> .text |> shout |> .text |> shout |> .text",
        " |> shout |> .text |> shout -> very_long_result // keep me\n",
        "  very_long_result |> route finished\n",
    );
    let printed = print(&parse(source)?);
    assert!(
        printed.contains("|> shout -> very_long_result // keep me\n"),
        "wrapped bind chain dropped its trailing comment:\n{printed}"
    );
    let reparsed =
        parse(&printed).map_err(|error| format!("canonical output failed to re-parse: {error}"))?;
    assert_eq!(printed, print(&reparsed), "not idempotent");
    Ok(())
}

#[test]
fn long_pipe_chains_break_before_each_stage() -> TestResult {
    let source = concat!(
        "//! Long pipe chain wrapping.\n",
        "workflow wrap\n",
        "  input extraordinarily_long_binding_name: [String]\n",
        "  outcome finished: type Out, route success\n",
        "\n",
        "type Out { text: String }\n",
        "\n",
        "worker w\n",
        "  action reticulate_all_available_splines(xs: [String]) -> [String]\n",
        "  action condense_everything_down(xs: [String]) -> Out\n",
        "\n",
        "step run\n",
        "  extraordinarily_long_binding_name |> reticulate_all_available_splines",
        " |> condense_everything_down -> condensed_result\n",
        "  condensed_result |> route finished\n",
    );
    let printed = print(&parse(source)?);
    assert!(
        printed.contains(concat!(
            "  extraordinarily_long_binding_name\n",
            "    |> reticulate_all_available_splines\n",
            "    |> condense_everything_down -> condensed_result\n",
        )),
        "chain did not wrap canonically:\n{printed}"
    );
    for line in printed.lines() {
        assert!(
            line.chars().count() <= 100,
            "line over 100 columns: {line:?}"
        );
    }
    assert_eq!(printed, print(&parse(&printed)?), "not idempotent");
    Ok(())
}
