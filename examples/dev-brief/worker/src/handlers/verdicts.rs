//! Pure formatting boundary for collected review-lens verdicts.
//!
//! This activity exposes no workflow disposition: AWL owns the review-round
//! decision. It only reproduces `dev_brief/verdicts.gleam`'s adverse evidence
//! lines and the final `"; "` join used by `last_adverse_evidence`.

use aion_worker::ActivityFailure;

use crate::types::{
    FormatVerdictEvidenceInput, FormatVerdictEvidenceOutput, LensVerdict, Overall, Severity,
};

/// Format one round's verdict evidence for developer loop-back and final-report
/// text. Unlike the git/gate handlers on the same node, formatting performs no
/// shell I/O.
///
/// # Errors
///
/// This pure formatter currently has no failure path. The result remains typed
/// like the other shell handlers so registration uses the same worker adapter.
pub fn format_verdict_evidence(
    input: FormatVerdictEvidenceInput,
) -> Result<FormatVerdictEvidenceOutput, ActivityFailure> {
    let FormatVerdictEvidenceInput { verdicts } = input;
    Ok(FormatVerdictEvidenceOutput {
        evidence: format_evidence(&verdicts),
    })
}

/// Reproduce `adverse_lines` followed by the workflow's `string.join(lines,
/// "; ")`. The derive-and-check calculation is used only to select the same
/// evidence lines as Gleam; no acceptance Boolean leaves this formatter.
pub(crate) fn format_evidence(verdicts: &[LensVerdict]) -> String {
    adverse_lines(verdicts).join("; ")
}

/// Reproduce the workflow's ordered `adverse_lines` before its final join.
pub(crate) fn adverse_lines(verdicts: &[LensVerdict]) -> Vec<String> {
    verdicts
        .iter()
        .filter(|verdict| emits_adverse_line(verdict))
        .map(format_adverse_line)
        .collect::<Vec<_>>()
}

fn emits_adverse_line(verdict: &LensVerdict) -> bool {
    let derived = if verdict
        .findings
        .iter()
        .any(|finding| finding.severity == Severity::Blocking)
    {
        Overall::Reject
    } else {
        Overall::Accept
    };
    let disagree = verdict.overall != derived;
    let missing_reason = (derived == Overall::Reject || verdict.overall == Overall::Reject)
        && verdict
            .reject_reason
            .as_deref()
            .is_none_or(|reason| reason.trim().is_empty());
    derived != Overall::Accept || disagree || missing_reason
}

fn format_adverse_line(verdict: &LensVerdict) -> String {
    let reason = verdict
        .reject_reason
        .as_deref()
        .unwrap_or("(no reject_reason given)");
    let blocking_titles = verdict
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Blocking)
        .map(|finding| finding.title.as_str())
        .collect::<Vec<_>>();
    let blocking = if blocking_titles.is_empty() {
        String::new()
    } else {
        format!(" [blocking: {}]", blocking_titles.join("; "))
    };
    format!("lens {} rejects: {reason}{blocking}", verdict.lens)
}

#[cfg(test)]
mod tests {
    use super::format_evidence;
    use crate::types::FormatVerdictEvidenceInput;

    /// Realistic scout-pack `LensVerdict` shapes pin byte parity with Gleam's
    /// `adverse_lines` plus the final semicolon join. Advisory findings remain
    /// out of the blocking-title suffix.
    #[test]
    fn realistic_verdicts_match_gleam_evidence_bytes() -> Result<(), serde_json::Error> {
        let input: FormatVerdictEvidenceInput = serde_json::from_str(
            r#"{"verdicts":[{"lens":"correctness","findings":[],"overall":"accept","reject_reason":null},{"lens":"regressions","findings":[{"severity":"blocking","title":"Public API was removed","evidence":"src/lib.rs:42 no longer exports parse"},{"severity":"advisory","title":"Add a migration note","evidence":"README has no upgrade section"}],"overall":"reject","reject_reason":"existing callers no longer compile"},{"lens":"brief_compliance","findings":[{"severity":"blocking","title":"Acceptance item 2 is missing","evidence":"no regression test covers absent gate"},{"severity":"blocking","title":"Return shape is bare","evidence":"handler returns a string"}],"overall":"reject","reject_reason":"the activity contract is incomplete"}]}"#,
        )?;

        assert_eq!(
            format_evidence(&input.verdicts),
            "lens regressions rejects: existing callers no longer compile \
             [blocking: Public API was removed]; lens brief_compliance rejects: \
             the activity contract is incomplete [blocking: Acceptance item 2 is missing; \
             Return shape is bare]"
        );
        Ok(())
    }

    /// The optional reject reason follows the absent-never-null transition law
    /// at this new boundary too.
    #[test]
    fn absent_and_null_reject_reason_format_identically() -> Result<(), serde_json::Error> {
        let absent: FormatVerdictEvidenceInput = serde_json::from_str(
            r#"{"verdicts":[{"lens":"correctness","findings":[{"severity":"blocking","title":"broken","evidence":"line 1"}],"overall":"reject"}]}"#,
        )?;
        let explicit_null: FormatVerdictEvidenceInput = serde_json::from_str(
            r#"{"verdicts":[{"lens":"correctness","findings":[{"severity":"blocking","title":"broken","evidence":"line 1"}],"overall":"reject","reject_reason":null}]}"#,
        )?;

        assert_eq!(
            format_evidence(&absent.verdicts),
            format_evidence(&explicit_null.verdicts)
        );
        Ok(())
    }

    /// The worker boundary accepts AWL named arguments and returns a record;
    /// neither side silently collapses to a bare collection or string.
    #[test]
    fn activity_wire_is_named_input_and_record_output() -> anyhow::Result<()> {
        assert!(serde_json::from_str::<FormatVerdictEvidenceInput>("[]").is_err());
        let input: FormatVerdictEvidenceInput = serde_json::from_str("{\"verdicts\":[]}")?;
        let output = super::format_verdict_evidence(input)?;

        assert_eq!(
            serde_json::to_value(output)?,
            serde_json::json!({ "evidence": "" })
        );
        Ok(())
    }
}
