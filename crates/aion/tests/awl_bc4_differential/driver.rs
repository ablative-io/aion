//! The differential driver: classify a fixture set into the lowering
//! intersection vs out-of-intersection refusals, build both backends, run
//! both, and accumulate the divergence report.

use std::collections::HashMap;

use aion_awl::mir::lower;
use serde_json::Value;

use crate::fixtures::{self, Loaded};
use crate::harness::{self, RefEntry};
use crate::report::Report;
use crate::run::{RunOutcome, run_package};

/// The classified fixture set: everything both backends accept, plus every
/// out-of-intersection refusal.
pub struct Classified {
    /// Reference-build inputs for the lowering fixtures, in input order.
    pub entries: Vec<RefEntry>,
    /// Direct `select`ed bytes per ratchet name.
    pub direct_bytes: HashMap<String, Vec<u8>>,
    /// Minimal schema-valid input per ratchet name.
    pub inputs: HashMap<String, Value>,
    /// Out-of-intersection refusals as `(ratchet name, refusal text)`.
    pub refusals: Vec<(String, String)>,
    /// Divergences discovered during classification (e.g. IR-12 name skew).
    pub classification_divergences: Vec<(String, String, String)>,
}

/// Classifies each covered fixture: a `lower` refusal (or an emit/select
/// failure) is out-of-intersection and recorded; a fixture both backends
/// accept is resolved to its reference-build inputs and direct bytes.
///
/// # Errors
///
/// Fails only on unexpected I/O (a fixture that cannot be read at all); every
/// refusal is captured, never surfaced as an error.
pub fn classify(names: &[String]) -> Result<Classified, Box<dyn std::error::Error>> {
    let mut classified = Classified {
        entries: Vec::new(),
        direct_bytes: HashMap::new(),
        inputs: HashMap::new(),
        refusals: Vec::new(),
        classification_divergences: Vec::new(),
    };
    for name in names {
        let loaded = fixtures::load(name)?;
        match lower(&loaded.document, Some(loaded.dir.as_path())) {
            Ok(module) => classify_lowering(&mut classified, name, &loaded, &module),
            Err(error) => classified.refusals.push((name.clone(), error.to_string())),
        }
    }
    Ok(classified)
}

/// Resolves one lowering fixture; a downstream emit/select failure is recorded
/// as an out-of-intersection refusal, and an entry-module name skew between the
/// two backends is recorded as an IR-12 divergence.
fn classify_lowering(
    classified: &mut Classified,
    name: &str,
    loaded: &Loaded,
    module: &aion_awl::mir::MirModule,
) {
    let entry = match harness::ref_entry(loaded) {
        Ok(entry) => entry,
        Err(error) => {
            classified
                .refusals
                .push((name.to_owned(), format!("reference emit refused: {error}")));
            return;
        }
    };
    let bytes = match harness::select_direct(module) {
        Ok(bytes) => bytes,
        Err(error) => {
            classified
                .refusals
                .push((name.to_owned(), format!("direct select refused: {error}")));
            return;
        }
    };
    if module.name != entry.entry_module {
        classified.classification_divergences.push((
            name.to_owned(),
            format!("direct module name `{}`", module.name),
            format!("reference entry module `{}`", entry.entry_module),
        ));
        return;
    }
    let input = fixtures::example_for_schema(&entry.input_schema);
    classified.inputs.insert(name.to_owned(), input);
    classified.direct_bytes.insert(name.to_owned(), bytes);
    classified.entries.push(entry);
}

/// Runs the full differential over a fixture set and returns the report.
///
/// # Errors
///
/// Fails on classification or reference-build errors (a genuine backend
/// defect); per-fixture run outcomes are folded into the report, not surfaced.
pub async fn run_differential(
    names: &[String],
    label: &str,
) -> Result<Report, Box<dyn std::error::Error>> {
    let classified = classify(names)?;
    let mut report = Report::default();
    for (fixture, text) in &classified.refusals {
        report.record_refusal(fixture, text.clone());
    }
    for (fixture, left, right) in &classified.classification_divergences {
        report.compare(
            fixture,
            &[Value::String(left.clone())],
            &[Value::String(right.clone())],
        );
    }

    let reference = harness::build_reference(&classified.entries, label)?;
    for entry in &classified.entries {
        differential_one(&mut report, &reference, &classified, entry).await;
    }
    Ok(report)
}

/// Runs one fixture through both backends and folds the outcome into the
/// report.
async fn differential_one(
    report: &mut Report,
    reference: &harness::ReferenceBuild,
    classified: &Classified,
    entry: &RefEntry,
) {
    let name = entry.name.as_str();
    let Some(reference_package) = reference.package(name) else {
        report.record_unsettled(name, "no reference package");
        return;
    };
    let Some(direct_bytes) = classified.direct_bytes.get(name) else {
        report.record_unsettled(name, "no direct bytes");
        return;
    };
    let direct_package = match harness::splice_direct(reference_package, direct_bytes) {
        Ok(package) => package,
        Err(error) => {
            report.record_unsettled(name, &format!("direct splice failed: {error}"));
            return;
        }
    };
    let input = classified.inputs.get(name).cloned().unwrap_or(Value::Null);
    let workflow_type = entry.entry_module.as_str();

    let reference_run = run_package(reference_package.clone(), workflow_type, &input).await;
    let direct_run = run_package(direct_package, workflow_type, &input).await;
    fold_runs(report, name, reference_run, direct_run);
}

/// Compares the two run outcomes and records the result.
fn fold_runs(
    report: &mut Report,
    name: &str,
    reference_run: Result<RunOutcome, Box<dyn std::error::Error>>,
    direct_run: Result<RunOutcome, Box<dyn std::error::Error>>,
) {
    match (reference_run, direct_run) {
        (Ok(reference), Ok(direct)) => {
            let reference_trail = match crate::trail_norm::normalized_trail(&reference.trail) {
                Ok(trail) => trail,
                Err(error) => {
                    report.record_unsettled(name, &format!("reference trail serialize: {error}"));
                    return;
                }
            };
            let direct_trail = match crate::trail_norm::normalized_trail(&direct.trail) {
                Ok(trail) => trail,
                Err(error) => {
                    report.record_unsettled(name, &format!("direct trail serialize: {error}"));
                    return;
                }
            };
            if reference.settled != direct.settled {
                report.compare(
                    name,
                    &[Value::String(format!("settled={}", reference.settled))],
                    &[Value::String(format!("settled={}", direct.settled))],
                );
                return;
            }
            let divergences = report.compare(name, &reference_trail, &direct_trail);
            if divergences > 0 {
                return;
            }
            if reference.settled {
                report.record_identical(name);
            } else {
                report.record_unsettled(name, "blocked at a durable timer/wait in both backends");
            }
        }
        (Err(reference_error), Err(direct_error)) => {
            report.record_unsettled(
                name,
                &format!(
                    "both backends failed to run: ref=({reference_error}) direct=({direct_error})"
                ),
            );
        }
        (Ok(_), Err(direct_error)) => {
            report.compare(
                name,
                &[Value::String(String::from("ran"))],
                &[Value::String(format!("run error: {direct_error}"))],
            );
        }
        (Err(reference_error), Ok(_)) => {
            report.compare(
                name,
                &[Value::String(format!("run error: {reference_error}"))],
                &[Value::String(String::from("ran"))],
            );
        }
    }
}
