//! The differential driver: classify a fixture set into the lowering
//! intersection vs out-of-intersection refusals, build both backends, run
//! both, and accumulate the divergence report.

use std::collections::HashMap;

use aion_awl::mir::lower;
use serde_json::Value;

use crate::fixtures::{self, Loaded};
use crate::harness::{self, RefEntry};
use crate::report::Report;
use crate::run::{Disposition, RunOutcome, run_package};

/// The classified fixture set: everything both backends accept, plus every
/// out-of-intersection refusal.
pub struct Classified {
    /// Reference-build inputs for the lowering fixtures, in input order.
    pub entries: Vec<RefEntry>,
    /// Direct `select`ed bytes per ratchet name.
    pub direct_bytes: HashMap<String, Vec<u8>>,
    /// Minimal schema-valid input per ratchet name.
    pub inputs: HashMap<String, Value>,
    /// Schema-valid canned activity results per ratchet name.
    pub action_results: HashMap<String, HashMap<String, String>>,
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
        action_results: HashMap::new(),
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
    let results = fixtures::action_results(&loaded.document, loaded.dir.as_path());
    classified.inputs.insert(name.to_owned(), input);
    classified.action_results.insert(name.to_owned(), results);
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

/// A fixture built through both backends, exposing the RELOADED spliced direct
/// package and its entry bytes so the ABI suite asserts against the bytes the
/// engine actually loads (BLOCKER 4), not fresh `select()` output.
pub struct SplicedFixture {
    /// Emitted entry-module / workflow-type name.
    pub entry_module: String,
    /// The `select`ed entry bytes (identical to the spliced entry beam).
    pub entry_bytes: Vec<u8>,
    /// The reloaded direct package (spliced entry over the SDK closure).
    pub direct_package: aion_package::Package,
    /// The minimal schema-valid start input.
    pub input: Value,
    /// The schema-valid canned activity results.
    pub action_results: HashMap<String, String>,
}

/// Builds the spliced direct package for each named fixture (ONE `gleam
/// build`). Every named fixture MUST lie in the lowering intersection; a
/// refusal or classification skew is an error, since the ABI suite pins fixtures
/// it has already established both backends accept.
///
/// # Errors
///
/// Fails when a named fixture refuses/skews, the reference build fails, or the
/// splice-integrity precondition is violated.
pub async fn build_spliced(
    names: &[String],
    label: &str,
) -> Result<Vec<SplicedFixture>, Box<dyn std::error::Error>> {
    let classified = classify(names)?;
    if !classified.refusals.is_empty() || !classified.classification_divergences.is_empty() {
        return Err(format!(
            "ABI fixtures must be in the intersection: refusals={:?} skews={:?}",
            classified.refusals, classified.classification_divergences
        )
        .into());
    }
    let reference = harness::build_reference(&classified.entries, label)?;
    let mut out = Vec::new();
    for entry in &classified.entries {
        let reference_package = reference
            .package(&entry.name)
            .ok_or_else(|| format!("no reference package for {}", entry.name))?;
        let entry_bytes = classified
            .direct_bytes
            .get(&entry.name)
            .ok_or_else(|| format!("no direct bytes for {}", entry.name))?
            .clone();
        let direct_package = harness::splice_direct(reference_package, &entry_bytes)?;
        out.push(SplicedFixture {
            entry_module: entry.entry_module.clone(),
            entry_bytes,
            direct_package,
            input: classified
                .inputs
                .get(&entry.name)
                .cloned()
                .unwrap_or(Value::Null),
            action_results: classified
                .action_results
                .get(&entry.name)
                .cloned()
                .unwrap_or_default(),
        });
    }
    Ok(out)
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
        report.record_infra(name, "no reference package produced");
        return;
    };
    let Some(direct_bytes) = classified.direct_bytes.get(name) else {
        report.record_infra(name, "no direct select bytes");
        return;
    };
    // The splice is mutation-sensitive by construction (BLOCKER 1): it fails
    // unless the reloaded direct package carries the select() bytes as its entry
    // module, differs from the reference entry, and hashes differently — so the
    // oracle can never silently compare a package to itself.
    let direct_package = match harness::splice_direct(reference_package, direct_bytes) {
        Ok(package) => package,
        Err(error) => {
            report.record_infra(name, &format!("splice integrity violation: {error}"));
            return;
        }
    };
    let input = classified.inputs.get(name).cloned().unwrap_or(Value::Null);
    let results = classified
        .action_results
        .get(name)
        .cloned()
        .unwrap_or_default();
    let workflow_type = entry.entry_module.as_str();

    let reference_run = run_package(
        reference_package.clone(),
        workflow_type,
        &input,
        results.clone(),
    )
    .await;
    let direct_run = run_package(direct_package, workflow_type, &input, results).await;
    fold_runs(report, name, reference_run, direct_run);
}

/// Compares the two run outcomes and records the result. Any run error, `Stuck`
/// disposition, or serialization failure is INFRA (a hard failure), never
/// quiesced. A backend disagreement on disposition is a divergence.
fn fold_runs(
    report: &mut Report,
    name: &str,
    reference_run: Result<RunOutcome, Box<dyn std::error::Error>>,
    direct_run: Result<RunOutcome, Box<dyn std::error::Error>>,
) {
    let (reference, direct) = match (reference_run, direct_run) {
        (Ok(reference), Ok(direct)) => (reference, direct),
        (reference_run, direct_run) => {
            report.record_infra(
                name,
                &format!(
                    "engine run error: ref={:?} direct={:?}",
                    reference_run.err().map(|error| error.to_string()),
                    direct_run.err().map(|error| error.to_string()),
                ),
            );
            return;
        }
    };
    if reference.disposition == Disposition::Stuck || direct.disposition == Disposition::Stuck {
        report.record_infra(
            name,
            &format!(
                "run never reached a terminal or a stable durable-timer park \
                 (ref={:?}, direct={:?})",
                reference.disposition, direct.disposition
            ),
        );
        return;
    }
    let reference_trail = match crate::trail_norm::normalized_trail(&reference.trail) {
        Ok(trail) => trail,
        Err(error) => {
            report.record_infra(name, &format!("reference trail serialize: {error}"));
            return;
        }
    };
    let direct_trail = match crate::trail_norm::normalized_trail(&direct.trail) {
        Ok(trail) => trail,
        Err(error) => {
            report.record_infra(name, &format!("direct trail serialize: {error}"));
            return;
        }
    };
    if reference.disposition != direct.disposition {
        report.compare(
            name,
            &[Value::String(format!(
                "disposition={:?}",
                reference.disposition
            ))],
            &[Value::String(format!(
                "disposition={:?}",
                direct.disposition
            ))],
        );
        return;
    }
    // The two backends must agree on the park kind too (a timer park on one
    // side and a signal park on the other would be a real behavioral split).
    if reference.disposition == Disposition::Parked
        && reference.timer_pending != direct.timer_pending
    {
        report.compare(
            name,
            &[Value::String(format!(
                "timer_pending={}",
                reference.timer_pending
            ))],
            &[Value::String(format!(
                "timer_pending={}",
                direct.timer_pending
            ))],
        );
        return;
    }
    if report.compare(name, &reference_trail, &direct_trail) > 0 {
        return;
    }
    match reference.disposition {
        Disposition::Completed => report.record_succeeded(name),
        Disposition::Failed | Disposition::Cancelled => report.record_failed(name),
        Disposition::Parked if reference.timer_pending => report.record_parked_timer(name),
        Disposition::Parked => report.record_parked_signal(name),
        // `Stuck` was handled above as infra.
        Disposition::Stuck => report.record_infra(name, "unreachable stuck classification"),
    }
}
