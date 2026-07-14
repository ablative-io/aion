//! Pure round-state packing boundary for the AWL-authored developer loop.
//!
//! AWL loops thread exactly one value, so this activity echoes the round's
//! report, gate, and verdicts while folding adverse and reset evidence into the
//! carried string. It performs no shell I/O and owns no workflow disposition.

use aion_worker::ActivityFailure;

use super::verdicts::adverse_lines;
use crate::types::{FoldRoundInput, FoldRoundOutput, LensVerdict};

/// Pack one completed review round into the value carried by the AWL loop.
///
/// # Errors
///
/// Returns a terminal input failure when any raw verdict cannot be decoded into
/// the typed view required by the shared adverse-evidence formatter.
pub fn fold_round(input: FoldRoundInput) -> Result<FoldRoundOutput, ActivityFailure> {
    let FoldRoundInput {
        report,
        gate,
        verdicts,
        prior_evidence,
        droppings,
    } = input;
    let typed_verdicts = decode_verdicts(&verdicts)?;
    let mut round_lines = adverse_lines(&typed_verdicts);
    if !droppings.is_empty() {
        round_lines.push(format!("reset droppings: {}", droppings.join(", ")));
    }
    let round_evidence = round_lines.join("; ");
    let evidence = smart_join(&prior_evidence, &round_evidence);

    Ok(FoldRoundOutput {
        report,
        gate,
        verdicts,
        evidence,
    })
}

fn decode_verdicts(verdicts: &[serde_json::Value]) -> Result<Vec<LensVerdict>, ActivityFailure> {
    verdicts
        .iter()
        .enumerate()
        .map(|(index, verdict)| {
            serde_json::from_value(verdict.clone()).map_err(|error| {
                ActivityFailure::terminal(format!(
                    "fold_round verdict at index {index} is invalid: {error}"
                ))
            })
        })
        .collect()
}

fn smart_join(prior_evidence: &str, round_evidence: &str) -> String {
    match (prior_evidence.is_empty(), round_evidence.is_empty()) {
        (_, true) => prior_evidence.to_owned(),
        (true, false) => round_evidence.to_owned(),
        (false, false) => format!("{prior_evidence}; {round_evidence}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_verdicts, fold_round};
    use crate::handlers::verdicts::adverse_lines;
    use crate::types::{FoldRoundInput, FormatVerdictEvidenceInput};

    const REALISTIC_INPUT: &str = r#"{
        "report": {
            "brief_id": "DB-42",
            "summary": "Added the pure round-state packing boundary.",
            "commits": ["7f39d20"],
            "acceptance_claims": [
                {
                    "criterion": "Evidence is carried across rounds",
                    "how": "fold_round smart-joins prior and current evidence"
                }
            ],
            "deviations": [
                {
                    "what": "No workflow edits in this worker change",
                    "why": "The brief is scoped to the worker activity"
                }
            ]
        },
        "gate": {
            "pass": true,
            "runs": [
                {
                    "name": "unit tests",
                    "argv": ["cargo", "test"],
                    "exit_code": 0,
                    "passed": true,
                    "output_tail": "test result: ok"
                }
            ],
            "diff": "diff --git a/src/lib.rs b/src/lib.rs",
            "diagnostics": "all 1 gate command(s) passed"
        },
        "verdicts": [
            {
                "lens": "correctness",
                "findings": [],
                "overall": "accept",
                "reject_reason": null
            },
            {
                "lens": "brief_compliance",
                "findings": [
                    {
                        "severity": "blocking",
                        "title": "Missing echo",
                        "evidence": "report was not returned"
                    },
                    {
                        "severity": "advisory",
                        "title": "Clarify docs",
                        "evidence": "the boundary could be described more directly"
                    }
                ],
                "overall": "reject",
                "reject_reason": "the carried state is incomplete"
            }
        ],
        "prior_evidence": "earlier gate failed",
        "droppings": [" M src/lib.rs", "?? scratch.txt"]
    }"#;

    fn realistic_input() -> Result<FoldRoundInput, serde_json::Error> {
        let input = serde_json::from_str(REALISTIC_INPUT)?;
        Ok(input)
    }

    fn folded_evidence(input: FoldRoundInput) -> anyhow::Result<String> {
        let output = fold_round(input)?;
        Ok(output.evidence)
    }

    /// `fold_round` consumes the formatter's ordered adverse lines directly,
    /// pinning byte parity with `format_verdict_evidence` for identical verdicts.
    #[test]
    fn verdict_lines_are_byte_identical_to_format_verdict_evidence() -> anyhow::Result<()> {
        let mut input = realistic_input()?;
        input.prior_evidence.clear();
        input.droppings.clear();
        let typed_verdicts = decode_verdicts(&input.verdicts)?;
        let formatted = crate::handlers::format_verdict_evidence(FormatVerdictEvidenceInput {
            verdicts: typed_verdicts.clone(),
        })?;
        let folded = fold_round(input)?;

        assert_eq!(
            adverse_lines(&typed_verdicts).join("; "),
            formatted.evidence
        );
        assert_eq!(folded.evidence, formatted.evidence);
        Ok(())
    }

    /// Empty sides never introduce a leading or trailing separator; two
    /// present sides receive exactly one `"; "` separator.
    #[test]
    fn evidence_fold_smart_joins_all_empty_and_present_combinations() -> anyhow::Result<()> {
        let mut round_only = realistic_input()?;
        round_only.prior_evidence.clear();
        round_only.droppings.clear();
        assert_eq!(
            folded_evidence(round_only)?,
            "lens brief_compliance rejects: the carried state is incomplete \
             [blocking: Missing echo]"
        );

        let mut prior_only = realistic_input()?;
        prior_only.verdicts.clear();
        prior_only.droppings.clear();
        assert_eq!(folded_evidence(prior_only)?, "earlier gate failed");

        let mut both = realistic_input()?;
        both.droppings.clear();
        assert_eq!(
            folded_evidence(both)?,
            "earlier gate failed; lens brief_compliance rejects: the carried state is incomplete \
             [blocking: Missing echo]"
        );

        let mut neither = realistic_input()?;
        neither.prior_evidence.clear();
        neither.verdicts.clear();
        neither.droppings.clear();
        assert_eq!(folded_evidence(neither)?, "");
        Ok(())
    }

    /// Reset droppings add one final line only when the collection is nonempty,
    /// preserving order and the exact comma-space format.
    #[test]
    fn reset_droppings_line_is_conditional_and_format_is_pinned() -> anyhow::Result<()> {
        let mut present = realistic_input()?;
        present.prior_evidence.clear();
        present.verdicts.clear();
        assert_eq!(
            folded_evidence(present)?,
            "reset droppings:  M src/lib.rs, ?? scratch.txt"
        );

        let mut absent = realistic_input()?;
        absent.prior_evidence.clear();
        absent.verdicts.clear();
        absent.droppings.clear();
        assert_eq!(folded_evidence(absent)?, "");
        Ok(())
    }

    /// Omitted report collections and an omitted verdict reason remain absent
    /// after folding rather than materializing as empty arrays or `null`.
    #[test]
    fn omitted_optional_fields_remain_omitted_in_echo() -> anyhow::Result<()> {
        const INPUT: &str = r#"{
            "report": {
                "brief_id": "DB-1",
                "summary": "minimal report",
                "acceptance_claims": []
            },
            "gate": {"pass": true, "runs": [], "diff": "", "diagnostics": "green"},
            "verdicts": [{"lens": "correctness", "findings": [], "overall": "accept"}],
            "prior_evidence": "",
            "droppings": []
        }"#;
        let original: serde_json::Value = serde_json::from_str(INPUT)?;
        let input: FoldRoundInput = serde_json::from_value(original.clone())?;
        let encoded = serde_json::to_value(fold_round(input)?)?;

        assert_eq!(encoded["report"], original["report"]);
        assert_eq!(encoded["gate"], original["gate"]);
        assert_eq!(encoded["verdicts"], original["verdicts"]);
        assert!(encoded["report"].get("commits").is_none());
        assert!(encoded["report"].get("deviations").is_none());
        assert!(encoded["verdicts"][0].get("reject_reason").is_none());
        Ok(())
    }

    /// Extension fields are opaque loop state: typed evidence derivation may
    /// ignore them, but the raw report and verdict values must retain them.
    #[test]
    fn unknown_report_and_verdict_fields_round_trip_unchanged() -> anyhow::Result<()> {
        const INPUT: &str = r#"{
            "report": {
                "brief_id": "DB-1",
                "summary": "extended report",
                "acceptance_claims": [],
                "x_custom": 1
            },
            "gate": {"pass": true, "runs": [], "diff": "", "diagnostics": "green"},
            "verdicts": [{
                "lens": "correctness",
                "findings": [],
                "overall": "accept",
                "x_custom": {"source": "reviewer"}
            }],
            "prior_evidence": "",
            "droppings": []
        }"#;
        let original: serde_json::Value = serde_json::from_str(INPUT)?;
        let input: FoldRoundInput = serde_json::from_value(original.clone())?;
        let encoded = serde_json::to_value(fold_round(input)?)?;

        assert_eq!(encoded["report"], original["report"]);
        assert_eq!(encoded["verdicts"], original["verdicts"]);
        assert_eq!(encoded["report"]["x_custom"], serde_json::json!(1));
        assert_eq!(
            encoded["verdicts"][0]["x_custom"],
            serde_json::json!({"source": "reviewer"})
        );
        Ok(())
    }

    /// The worker boundary still requires AWL named arguments rather than a
    /// bare positional collection.
    #[test]
    fn wire_requires_named_object_input() -> anyhow::Result<()> {
        assert!(serde_json::from_str::<FoldRoundInput>("[]").is_err());
        let input: FoldRoundInput = serde_json::from_str(
            r#"{
                "report": {},
                "gate": {},
                "verdicts": [],
                "prior_evidence": "",
                "droppings": []
            }"#,
        )?;
        let output = fold_round(input)?;

        assert_eq!(output.evidence, "");
        Ok(())
    }

    /// Raw passthrough never becomes a silent fallback: every verdict must
    /// also decode into the typed view required for evidence formatting.
    #[test]
    fn invalid_typed_verdict_view_fails_the_activity() -> anyhow::Result<()> {
        let input: FoldRoundInput = serde_json::from_str(
            r#"{
                "report": {},
                "gate": {},
                "verdicts": [{"lens": "correctness", "overall": "accept"}],
                "prior_evidence": "",
                "droppings": []
            }"#,
        )?;

        assert!(fold_round(input).is_err());
        Ok(())
    }
}
