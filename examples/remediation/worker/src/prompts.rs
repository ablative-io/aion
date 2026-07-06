//! Per-role prompt assembly: {the role's profile markdown} + {the per-run
//! context block carrying the activity's structured input JSON}.
//!
//! DELIBERATELY SEPARATED AND DUMB (task contract): the prompt INTERFACE —
//! which sections in which order — will be aligned with the profile author
//! (Scott, DECISIONS.md D3 convergence point), so each role has exactly one
//! obvious function whose body IS the interface. No templating engine, no
//! shared cleverness beyond the one two-section joiner they all read as.
//!
//! The context JSON is the workflow's activity input verbatim (already
//! recommendation-stripped for the test-author at the codec layer); nothing
//! is parsed or re-projected here — a lossy re-rendering would be a second
//! place for the contract to drift.

/// The signature every role's assembly function shares:
/// `(profile_markdown, context_json) -> full prompt`.
pub type AssembleFn = fn(&str, &str) -> String;

/// The one joiner: profile first (the doctrine), then a titled run-context
/// section carrying the input JSON in a fenced block.
fn profile_then_context(profile: &str, context_title: &str, context_json: &str) -> String {
    format!(
        "{}\n\n---\n\n## {}\n\nThe JSON below is this run's structured context. \
         Work from these structured artifacts — never from a prose summary of \
         them.\n\n```json\n{}\n```\n",
        profile.trim_end(),
        context_title,
        context_json
    )
}

/// The test-author prompt: profile, then {brief + recommendation-stripped
/// ledger entries}. The strip already happened at the workflow's codec layer;
/// the title says so, so the agent knows the withholding is by design.
#[must_use]
pub fn test_author(profile: &str, context_json: &str) -> String {
    profile_then_context(
        profile,
        "Run context: brief + ledger entries (recommendations withheld by design)",
        context_json,
    )
}

/// The developer prompt: profile, then {brief + FULL ledger entries + test
/// manifest + gate-1 evidence, and on loop-back rounds the adverse verdict
/// and/or failing gate-2 outcome}.
#[must_use]
pub fn developer(profile: &str, context_json: &str) -> String {
    profile_then_context(
        profile,
        "Run context: brief + full ledger entries + failing tests \
         (+ verdict/gate feedback when cycling)",
        context_json,
    )
}

/// The verifier prompt: profile, then {findings + diff + fix report + test
/// manifest} (DESIGN.md Stage 3's declared inputs).
#[must_use]
pub fn verifier(profile: &str, context_json: &str) -> String {
    profile_then_context(
        profile,
        "Run context: original findings + diff + fix report + test manifest",
        context_json,
    )
}

/// The re-auditor prompt: profile, then {the original area finder prompt +
/// the wave's touched files} (Stage 4; served for the wave-level flow to
/// come).
#[must_use]
pub fn re_auditor(profile: &str, context_json: &str) -> String {
    profile_then_context(
        profile,
        "Run context: original area finder prompt + touched files",
        context_json,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    const PROFILE: &str = "# Role\n\nDoctrine body.";
    const CONTEXT: &str = "{\"brief\":{\"id\":\"B-1\"}}";

    /// Every role: profile first, context JSON verbatim after, fenced.
    #[test]
    fn every_role_assembles_profile_then_context() {
        for assemble in [
            super::test_author,
            super::developer,
            super::verifier,
            super::re_auditor,
        ] {
            let prompt = assemble(PROFILE, CONTEXT);
            let profile_at = prompt.find("Doctrine body.").expect("profile present");
            let context_at = prompt.find(CONTEXT).expect("context present verbatim");
            assert!(
                profile_at < context_at,
                "the profile must precede the context"
            );
            assert!(
                prompt.contains("```json"),
                "context rides in a fenced block"
            );
        }
    }

    #[test]
    fn the_test_author_section_declares_the_withholding() {
        let prompt = super::test_author(PROFILE, CONTEXT);
        assert!(
            prompt.contains("recommendations withheld by design"),
            "prompt was: {prompt}"
        );
    }

    #[test]
    fn the_verifier_section_names_its_declared_inputs() {
        let prompt = super::verifier(PROFILE, CONTEXT);
        assert!(prompt.contains("original findings + diff + fix report + test manifest"));
    }

    #[test]
    fn context_json_is_never_reprojected() {
        // A context containing text that LOOKS like a field must survive
        // byte-for-byte: assembly is concatenation, not templating.
        let tricky = "{\"detail\":\"contains {workflow_id} and ```\"}";
        let prompt = super::developer(PROFILE, tricky);
        assert!(prompt.contains(tricky));
    }
}
