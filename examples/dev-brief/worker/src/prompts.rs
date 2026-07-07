//! Per-role prompt assembly: {the role's profile markdown} + {the per-run
//! context block carrying the activity's structured input JSON}.
//!
//! DELIBERATELY SEPARATED AND DUMB (the remediation-worker discipline, kept):
//! each role has exactly one obvious function whose body IS the interface. No
//! templating engine, no shared cleverness beyond the one two-section joiner
//! they all read as.
//!
//! The context JSON is the workflow's activity input verbatim; nothing is
//! parsed or re-projected here — a lossy re-rendering would be a second place
//! for the contract to drift.

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

/// The developer prompt: profile, then {brief, and on loop-back rounds the
/// failing gate outcome and/or the adverse lens verdicts being addressed}.
#[must_use]
pub fn developer(profile: &str, context_json: &str) -> String {
    profile_then_context(
        profile,
        "Run context: brief (+ gate/verdict feedback when cycling)",
        context_json,
    )
}

/// The review-lens prompt: profile, then {lens charter + brief + diff + dev
/// report + gate evidence}.
#[must_use]
pub fn review_lens(profile: &str, context_json: &str) -> String {
    profile_then_context(
        profile,
        "Run context: lens charter + brief + diff + dev report + gate evidence",
        context_json,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    const PROFILE: &str = "# Role\n\nDoctrine body.";
    const CONTEXT: &str = "{\"brief\":{\"id\":\"DB-1\"}}";

    /// Every role: profile first, context JSON verbatim after, fenced.
    #[test]
    fn every_role_assembles_profile_then_context() {
        for assemble in [super::developer, super::review_lens] {
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
    fn context_json_is_never_reprojected() {
        // A context containing text that LOOKS like a field must survive
        // byte-for-byte: assembly is concatenation, not templating.
        let tricky = "{\"detail\":\"contains {workflow_id} and ```\"}";
        let prompt = super::developer(PROFILE, tricky);
        assert!(prompt.contains(tricky));
    }

    /// STANDING CONTRACT (kept from the remediation worker): the profile
    /// markdown reaches the prompt byte-identical from disk — no trimming,
    /// reflowing, or normalization beyond removing TRAILING whitespace.
    #[test]
    fn profile_markdown_survives_byte_identical_up_to_trailing_trim() {
        let odd_profile = "# Role   with   odd   spacing\n\
                           \n\
                           \t- a tab-indented bullet\n\
                           line one  \n\
                           line two\twith\ttabs\n\
                           \n\
                           ```rust\n\
                           fn keep_me() {}   // trailing spaces inside fences  \n\
                           ```\n\
                           \n\
                           em-dash — and «unicode» stay\n\n\n";
        for assemble in [super::developer, super::review_lens] {
            let prompt = assemble(odd_profile, "{}");
            let expected = odd_profile.trim_end();
            assert!(
                prompt.starts_with(expected),
                "the profile must open the prompt byte-identical (up to the \
                 trailing-whitespace trim); prompt was:\n{prompt}"
            );
            assert!(
                prompt[expected.len()..].starts_with("\n\n---\n\n"),
                "the separator must follow the untouched profile"
            );
        }
    }
}
