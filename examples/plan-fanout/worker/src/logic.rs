//! The CODE (non-agent) steps of plan-fanout, as pure functions plus their serde
//! wire types — and the unit tests that pin their behaviour.
//!
//! Three code steps back the three plain registry activities:
//!
//! - [`validate_plan`] — DAG validation (unknown / duplicate ids, self-loops,
//!   cycles), reviewer-count clamping to `1..=3`, the unit-count cap, and
//!   topological LAYERING. It never errors: an invalid plan comes back
//!   `accepted: false` with a human `reason`, so the workflow always gets a
//!   decodable [`ValidatedPlan`] and can complete with a terminal report.
//! - [`tally`] — the MAJORITY verdict over one unit's M reviews. A unit is
//!   blocked iff a STRICT majority of its reviewers returned `blockers`
//!   (`blocker_count * 2 > total`), so M=1 blocks on one blocker, 2-of-3 blocks a
//!   three-reviewer unit, and a 1-1 tie on two reviewers does NOT block.
//! - [`integrate`] — collect every settled unit into the final run report,
//!   extracting each unit's `work_product` from its dev output and computing the
//!   summary counts.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One blocking defect with location evidence. Shared across the review, tally,
/// and report shapes — one type, one wire contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blocker {
    pub file: String,
    pub line: i64,
    pub issue: String,
}

// --- validate_plan ----------------------------------------------------------

/// One unit as the planner emitted it (parsed from the plan JSON).
#[derive(Debug, Clone, Deserialize)]
pub struct PlanUnit {
    pub unit_id: String,
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// The planner agent's full decomposition.
#[derive(Debug, Clone, Deserialize)]
pub struct PlanOutput {
    #[serde(default)]
    pub units: Vec<PlanUnit>,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub recommended_reviewers_per_unit: i64,
}

/// A unit echoed into the validated plan (same fields, always fully populated).
#[derive(Debug, Clone, Serialize)]
pub struct ValidatedUnit {
    pub unit_id: String,
    pub goal: String,
    pub inputs: Vec<String>,
    pub depends_on: Vec<String>,
}

/// The `validate_plan` result handed back to the workflow.
#[derive(Debug, Serialize)]
pub struct ValidatedPlan {
    pub accepted: bool,
    pub reason: String,
    pub rationale: String,
    pub reviewers_per_unit: i64,
    pub max_fix_rounds: i64,
    pub layers: Vec<Vec<String>>,
    pub units: Vec<ValidatedUnit>,
}

/// Clamp the planner's recommended reviewer count into the supported `1..=3`.
pub fn clamp_reviewers(recommended: i64) -> i64 {
    recommended.clamp(1, 3)
}

/// Validate and layer the plan. Never errors — an unacceptable plan returns
/// `accepted: false` with a `reason`.
pub fn validate_plan(plan: &PlanOutput, max_units: i64, max_fix_rounds: i64) -> ValidatedPlan {
    let reviewers = clamp_reviewers(plan.recommended_reviewers_per_unit);
    let reject = |reason: String| ValidatedPlan {
        accepted: false,
        reason,
        rationale: plan.rationale.clone(),
        reviewers_per_unit: reviewers,
        max_fix_rounds,
        layers: Vec::new(),
        units: Vec::new(),
    };

    if plan.units.is_empty() {
        return reject("plan has no units".to_owned());
    }
    if plan.units.len() as i64 > max_units {
        return reject(format!(
            "plan has {} units, exceeds the max_units cap of {max_units}",
            plan.units.len()
        ));
    }

    let mut ids: BTreeSet<String> = BTreeSet::new();
    for unit in &plan.units {
        if !ids.insert(unit.unit_id.clone()) {
            return reject(format!("duplicate unit_id `{}`", unit.unit_id));
        }
    }
    for unit in &plan.units {
        for dependency in &unit.depends_on {
            if dependency == &unit.unit_id {
                return reject(format!("unit `{}` depends on itself", unit.unit_id));
            }
            if !ids.contains(dependency) {
                return reject(format!(
                    "unit `{}` depends on unknown unit `{dependency}`",
                    unit.unit_id
                ));
            }
        }
    }

    match topological_layers(&plan.units) {
        Ok(layers) => ValidatedPlan {
            accepted: true,
            reason: String::new(),
            rationale: plan.rationale.clone(),
            reviewers_per_unit: reviewers,
            max_fix_rounds,
            layers,
            units: plan
                .units
                .iter()
                .map(|unit| ValidatedUnit {
                    unit_id: unit.unit_id.clone(),
                    goal: unit.goal.clone(),
                    inputs: unit.inputs.clone(),
                    depends_on: unit.depends_on.clone(),
                })
                .collect(),
        },
        Err(reason) => reject(reason),
    }
}

/// Kahn's algorithm, emitting one LAYER per round (all currently-ready units)
/// preserving plan order within a layer. An empty ready set with units left over
/// is a dependency cycle.
fn topological_layers(units: &[PlanUnit]) -> Result<Vec<Vec<String>>, String> {
    let order: Vec<String> = units.iter().map(|unit| unit.unit_id.clone()).collect();
    let mut indegree: BTreeMap<String, usize> = units
        .iter()
        .map(|unit| (unit.unit_id.clone(), unit.depends_on.len()))
        .collect();
    let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for unit in units {
        for dependency in &unit.depends_on {
            dependents
                .entry(dependency.clone())
                .or_default()
                .push(unit.unit_id.clone());
        }
    }

    let mut emitted: BTreeSet<String> = BTreeSet::new();
    let mut layers: Vec<Vec<String>> = Vec::new();
    while emitted.len() < units.len() {
        let ready: Vec<String> = order
            .iter()
            .filter(|id| !emitted.contains(*id) && indegree[*id] == 0)
            .cloned()
            .collect();
        if ready.is_empty() {
            let remaining: Vec<String> = order
                .iter()
                .filter(|id| !emitted.contains(*id))
                .cloned()
                .collect();
            return Err(format!(
                "dependency cycle among units: {}",
                remaining.join(", ")
            ));
        }
        for id in &ready {
            emitted.insert(id.clone());
            if let Some(children) = dependents.get(id) {
                for child in children {
                    if let Some(degree) = indegree.get_mut(child) {
                        *degree = degree.saturating_sub(1);
                    }
                }
            }
        }
        layers.push(ready);
    }
    Ok(layers)
}

// --- tally ------------------------------------------------------------------

/// One reviewer's verdict, parsed from a review agent's output. The reviewer
/// also echoes `unit_id`, but tally is called with the unit already known, so
/// that field is ignored here (serde drops unmodelled fields).
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewOutput {
    pub verdict: String,
    #[serde(default)]
    pub blockers: Vec<Blocker>,
}

/// The majority verdict for one unit.
#[derive(Debug, Serialize)]
pub struct TallyResult {
    pub unit_id: String,
    pub blocked: bool,
    pub pass_count: i64,
    pub blocker_count: i64,
    pub blockers: Vec<Blocker>,
}

/// Compute the MAJORITY verdict. Blocked iff a STRICT majority of reviewers
/// returned `blockers`. Aggregates the blockers from every blocking reviewer.
pub fn tally(unit_id: &str, reviews: &[ReviewOutput]) -> TallyResult {
    let total = reviews.len() as i64;
    let blocker_count = reviews
        .iter()
        .filter(|review| review.verdict == "blockers")
        .count() as i64;
    let pass_count = reviews
        .iter()
        .filter(|review| review.verdict == "pass")
        .count() as i64;
    let blocked = blocker_count * 2 > total;
    let blockers = reviews
        .iter()
        .filter(|review| review.verdict == "blockers")
        .flat_map(|review| review.blockers.iter().cloned())
        .collect();
    TallyResult {
        unit_id: unit_id.to_owned(),
        blocked,
        pass_count,
        blocker_count,
        blockers,
    }
}

// --- integrate --------------------------------------------------------------

/// The dev output, parsed only far enough to lift out the work product.
#[derive(Debug, Clone, Deserialize)]
struct DevOutput {
    #[serde(default)]
    work_product: String,
}

/// One settled unit handed to integrate by the workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrateUnit {
    pub unit_id: String,
    #[serde(default)]
    pub goal: String,
    pub verdict: String,
    pub rounds_used: i64,
    /// The dev agent's full JSON output, carried as a string.
    #[serde(default)]
    pub dev_output: String,
    #[serde(default)]
    pub blockers: Vec<Blocker>,
}

/// The integrate activity input assembled by the workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrateInput {
    pub disposition: String,
    #[serde(default)]
    pub rationale: String,
    pub reviewers_per_unit: i64,
    pub max_fix_rounds: i64,
    #[serde(default)]
    pub units: Vec<IntegrateUnit>,
}

#[derive(Debug, Serialize)]
struct ReportUnit {
    unit_id: String,
    goal: String,
    verdict: String,
    rounds_used: i64,
    work_product: String,
    blockers: Vec<Blocker>,
}

#[derive(Debug, Serialize)]
struct ReportSummary {
    unit_count: i64,
    passed: i64,
    blocked: i64,
    reviewers_per_unit: i64,
    max_fix_rounds: i64,
}

/// The final structured run report.
#[derive(Debug, Serialize)]
pub struct Report {
    disposition: String,
    rationale: String,
    summary: ReportSummary,
    units: Vec<ReportUnit>,
}

/// Collect the settled units into the run report, lifting each work product out
/// of its dev output and computing the summary counts.
pub fn integrate(input: IntegrateInput) -> Report {
    let unit_count = input.units.len() as i64;
    let passed = input
        .units
        .iter()
        .filter(|unit| unit.verdict == "pass")
        .count() as i64;
    let blocked = unit_count - passed;
    let units = input
        .units
        .into_iter()
        .map(|unit| {
            let work_product = serde_json::from_str::<DevOutput>(&unit.dev_output)
                .map(|dev| dev.work_product)
                .unwrap_or_default();
            ReportUnit {
                unit_id: unit.unit_id,
                goal: unit.goal,
                verdict: unit.verdict,
                rounds_used: unit.rounds_used,
                work_product,
                blockers: unit.blockers,
            }
        })
        .collect();
    Report {
        disposition: input.disposition,
        rationale: input.rationale,
        summary: ReportSummary {
            unit_count,
            passed,
            blocked,
            reviewers_per_unit: input.reviewers_per_unit,
            max_fix_rounds: input.max_fix_rounds,
        },
        units,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(id: &str, deps: &[&str]) -> PlanUnit {
        PlanUnit {
            unit_id: id.to_owned(),
            goal: format!("do {id}"),
            inputs: Vec::new(),
            depends_on: deps.iter().map(|d| (*d).to_owned()).collect(),
        }
    }

    fn plan(units: Vec<PlanUnit>, reviewers: i64) -> PlanOutput {
        PlanOutput {
            units,
            rationale: "because".to_owned(),
            recommended_reviewers_per_unit: reviewers,
        }
    }

    fn review(verdict: &str) -> ReviewOutput {
        ReviewOutput {
            verdict: verdict.to_owned(),
            blockers: if verdict == "blockers" {
                vec![Blocker {
                    file: "work_product".to_owned(),
                    line: 1,
                    issue: "bad".to_owned(),
                }]
            } else {
                Vec::new()
            },
        }
    }

    // --- reviewer-count clamping ---

    #[test]
    fn reviewer_count_clamps_into_one_to_three() {
        assert_eq!(clamp_reviewers(0), 1, "zero clamps up to the floor");
        assert_eq!(clamp_reviewers(-4), 1, "negative clamps up to the floor");
        assert_eq!(clamp_reviewers(1), 1);
        assert_eq!(clamp_reviewers(2), 2);
        assert_eq!(clamp_reviewers(3), 3);
        assert_eq!(clamp_reviewers(5), 3, "over-max clamps down to the ceiling");
    }

    #[test]
    fn validate_clamps_reviewers_even_on_an_accepted_plan() {
        let validated = validate_plan(&plan(vec![unit("u1", &[])], 9), 8, 2);
        assert!(validated.accepted);
        assert_eq!(validated.reviewers_per_unit, 3, "9 recommended clamps to 3");
    }

    // --- unit-count cap ---

    #[test]
    fn unit_count_over_cap_is_rejected() {
        let validated = validate_plan(
            &plan(vec![unit("u1", &[]), unit("u2", &[]), unit("u3", &[])], 2),
            2,
            2,
        );
        assert!(!validated.accepted);
        assert!(
            validated.reason.contains("exceeds"),
            "reason names the cap breach: {}",
            validated.reason
        );
        assert!(validated.layers.is_empty());
    }

    #[test]
    fn unit_count_at_cap_is_accepted() {
        let validated = validate_plan(&plan(vec![unit("u1", &[]), unit("u2", &[])], 2), 2, 2);
        assert!(validated.accepted, "count == cap is fine");
    }

    // --- DAG validation ---

    #[test]
    fn two_cycle_is_rejected() {
        let validated = validate_plan(
            &plan(vec![unit("u1", &["u2"]), unit("u2", &["u1"])], 2),
            8,
            2,
        );
        assert!(!validated.accepted);
        assert!(
            validated.reason.contains("cycle"),
            "reason names the cycle: {}",
            validated.reason
        );
    }

    #[test]
    fn three_cycle_is_rejected() {
        let validated = validate_plan(
            &plan(
                vec![unit("a", &["c"]), unit("b", &["a"]), unit("c", &["b"])],
                3,
            ),
            8,
            2,
        );
        assert!(!validated.accepted);
        assert!(validated.reason.contains("cycle"));
    }

    #[test]
    fn unknown_dependency_is_rejected() {
        let validated = validate_plan(&plan(vec![unit("u1", &["ghost"])], 1), 8, 2);
        assert!(!validated.accepted);
        assert!(validated.reason.contains("unknown"));
    }

    #[test]
    fn self_dependency_is_rejected() {
        let validated = validate_plan(&plan(vec![unit("u1", &["u1"])], 1), 8, 2);
        assert!(!validated.accepted);
        assert!(validated.reason.contains("itself"));
    }

    #[test]
    fn duplicate_unit_id_is_rejected() {
        let validated = validate_plan(&plan(vec![unit("u1", &[]), unit("u1", &[])], 2), 8, 2);
        assert!(!validated.accepted);
        assert!(validated.reason.contains("duplicate"));
    }

    // --- topological layering ---

    #[test]
    fn independent_units_form_one_layer() {
        let validated = validate_plan(&plan(vec![unit("u1", &[]), unit("u2", &[])], 2), 8, 2);
        assert!(validated.accepted);
        assert_eq!(
            validated.layers,
            vec![vec!["u1".to_owned(), "u2".to_owned()]]
        );
    }

    #[test]
    fn a_chain_forms_one_unit_per_layer() {
        let validated = validate_plan(
            &plan(
                vec![unit("u1", &[]), unit("u2", &["u1"]), unit("u3", &["u2"])],
                3,
            ),
            8,
            2,
        );
        assert!(validated.accepted);
        assert_eq!(
            validated.layers,
            vec![
                vec!["u1".to_owned()],
                vec!["u2".to_owned()],
                vec!["u3".to_owned()],
            ]
        );
    }

    #[test]
    fn a_diamond_layers_by_depth() {
        // u1 -> {u2, u3} -> u4
        let validated = validate_plan(
            &plan(
                vec![
                    unit("u1", &[]),
                    unit("u2", &["u1"]),
                    unit("u3", &["u1"]),
                    unit("u4", &["u2", "u3"]),
                ],
                4,
            ),
            8,
            2,
        );
        assert!(validated.accepted);
        assert_eq!(
            validated.layers,
            vec![
                vec!["u1".to_owned()],
                vec!["u2".to_owned(), "u3".to_owned()],
                vec!["u4".to_owned()],
            ]
        );
    }

    // --- majority verdict ---

    #[test]
    fn majority_with_one_reviewer() {
        assert!(
            !tally("u", &[review("pass")]).blocked,
            "M=1 pass -> not blocked"
        );
        assert!(
            tally("u", &[review("blockers")]).blocked,
            "M=1 blockers -> blocked"
        );
    }

    #[test]
    fn majority_with_two_reviewers_needs_both() {
        assert!(
            tally("u", &[review("blockers"), review("blockers")]).blocked,
            "2-of-2 blocks"
        );
        assert!(
            !tally("u", &[review("blockers"), review("pass")]).blocked,
            "a 1-1 tie does NOT block"
        );
        assert!(
            !tally("u", &[review("pass"), review("pass")]).blocked,
            "0-2 passes"
        );
    }

    #[test]
    fn majority_with_three_reviewers_is_two_of_three() {
        assert!(
            tally(
                "u",
                &[review("blockers"), review("blockers"), review("pass")]
            )
            .blocked,
            "2-of-3 blocks"
        );
        assert!(
            !tally("u", &[review("blockers"), review("pass"), review("pass")]).blocked,
            "1-of-3 does not block"
        );
        assert!(
            tally(
                "u",
                &[review("blockers"), review("blockers"), review("blockers")]
            )
            .blocked,
            "3-of-3 blocks"
        );
        assert!(
            !tally("u", &[review("pass"), review("pass"), review("pass")]).blocked,
            "0-of-3 passes"
        );
    }

    #[test]
    fn tally_counts_and_aggregates_blockers() {
        let result = tally(
            "u",
            &[review("blockers"), review("blockers"), review("pass")],
        );
        assert_eq!(result.blocker_count, 2);
        assert_eq!(result.pass_count, 1);
        assert_eq!(
            result.blockers.len(),
            2,
            "one blocker from each blocking reviewer"
        );
    }

    // --- integrate ---

    #[test]
    fn integrate_lifts_work_product_and_counts() {
        let input = IntegrateInput {
            disposition: "completed".to_owned(),
            rationale: "why".to_owned(),
            reviewers_per_unit: 2,
            max_fix_rounds: 2,
            units: vec![
                IntegrateUnit {
                    unit_id: "u1".to_owned(),
                    goal: "g1".to_owned(),
                    verdict: "pass".to_owned(),
                    rounds_used: 1,
                    dev_output: r#"{"unit_id":"u1","summary":"s","work_product":"THE BODY"}"#
                        .to_owned(),
                    blockers: Vec::new(),
                },
                IntegrateUnit {
                    unit_id: "u2".to_owned(),
                    goal: "g2".to_owned(),
                    verdict: "blockers".to_owned(),
                    rounds_used: 3,
                    dev_output: r#"{"work_product":"OTHER"}"#.to_owned(),
                    blockers: vec![Blocker {
                        file: "work_product".to_owned(),
                        line: 2,
                        issue: "x".to_owned(),
                    }],
                },
            ],
        };
        let report = integrate(input);
        assert_eq!(report.summary.unit_count, 2);
        assert_eq!(report.summary.passed, 1);
        assert_eq!(report.summary.blocked, 1);
        assert_eq!(report.units[0].work_product, "THE BODY");
        assert_eq!(report.units[1].work_product, "OTHER");
    }
}
