//! The composed agent harness for plan-fanout's three AGENT roles.
//!
//! ## Why a custom harness at all
//!
//! plan-fanout fans the SAME agent activity type out N times at runtime (one
//! `dev` per unit, M `review`s per unit). The first-party [`NornHarness`]
//! templating substitutes exactly two run-identity placeholders into its
//! configured args — `{workflow_id}` and `{activity_type}` — and nothing else
//! (no `{activity_id}`, no per-input token). So a fixed
//! `--session-id {workflow_id}-{activity_type}` would give EVERY member of a
//! same-type fan-out the SAME session id: all the dev agents would collide on one
//! resumed session, defeating parallelism and the "independent reviewer session"
//! requirement.
//!
//! The SDK also bakes one system prompt + one output schema into a harness at
//! construction, but plan-fanout has THREE roles (planner, dev, reviewer) each
//! needing its own instructions and its own `--output-schema`.
//!
//! ## What this harness does
//!
//! [`RoleDispatchHarness`] is a thin composition-root wrapper implementing the
//! neutral [`DynAgentHarness`] contract. Per attempt it:
//!
//! 1. reads `spec.activity_type` to pick the ROLE (planner / dev / reviewer) and
//!    its role-specific system prompt + embedded output schema;
//! 2. derives a DISTINCT `--session-id` from the run — `{workflow_id}-{hint}`,
//!    where `hint` is the `session_hint` field the workflow put in the activity
//!    input (`{unit_id}-dev-r{round}`, `{unit_id}-review-r{round}-{index}`), so
//!    every fan-out member is its own resumable session; and
//! 3. builds a fresh [`NornHarness`] with those args and DELEGATES `start_dyn` to
//!    it — so the run is still a real driven Norn session with a live transcript
//!    and inject/cancel interventions; nothing about the observable agent path is
//!    given up.
//!
//! This is the piece the SDK does not provide today: dynamic-N fan-out of the
//! SAME agent type with a distinct, resumable session per member, on the driven
//! (intervenable) path. The session id is derived from the activity INPUT rather
//! than a placeholder, which is the only seam that carries per-member identity.

use aion_integration_norn::NornHarness;
use aion_integrations::{AgentRunSpec, DynAgentHarness, DynAgentSession, HarnessError};
use async_trait::async_trait;

/// Planner system prompt. The design document arrives as the `design_document`
/// field (a JSON string) and the unit cap as `max_units`.
const PLAN_SYSTEM_PROMPT: &str = "\
You are a planning agent. The user message is a JSON object with `design_document` \
(a design document as a JSON string: `title`, `summary`, `requirements` each with \
`id`/`statement`/`acceptance`, and optional `non_goals`) and `max_units` (the \
maximum number of units you may emit). Decompose the design into the SMALLEST set \
of coherent, independently-implementable units that together cover every \
requirement — never more than `max_units`. For each unit set `unit_id` (a short \
unique slug), `goal` (what it must produce), `inputs` (the requirements, facts, or \
upstream unit results it needs), and `depends_on` (the unit_ids that must finish \
first; empty when the unit is independent — prefer independence so units run in \
parallel). Set `recommended_reviewers_per_unit` between 1 and 3 by how risky the \
unit is. Return only the structured decomposition; do not invent requirements.";

/// Dev system prompt. The task spec arrives as the activity input JSON.
const DEV_SYSTEM_PROMPT: &str = "\
You are a development agent working ONE unit. The user message is a JSON task spec \
with `unit_id`, `goal`, `inputs`, and — on a fix round — `prior_blockers` you MUST \
resolve. Produce the deliverable as TEXT in `work_product` (the analysis, the patch \
text, or the document body that satisfies the goal) plus a one-or-two-sentence \
`summary`, and echo `unit_id`. If `prior_blockers` is non-empty, address every one \
of them. Ground everything in the provided inputs; do not invent facts. Return only \
the structured dev output.";

/// Reviewer system prompt. The unit goal and the dev output arrive in the input.
const REVIEW_SYSTEM_PROMPT: &str = "\
You are an INDEPENDENT reviewer. The user message is a JSON object with the unit \
`goal` and `dev_output` (the dev agent's JSON result as a string, containing its \
`work_product`). Judge ONLY whether the work_product meets the goal — you share no \
context with other reviewers. Set `verdict` to `pass` or `blockers` and echo \
`unit_id`. When `blockers`, list each BLOCKING defect with `file` (cite \
`work_product` for an inline text deliverable), `line` (1-based line within the \
work_product), and `issue` (the problem and what must change). The `blockers` array \
MUST be empty for `pass` and non-empty for `blockers`. Be strict but fair: only \
block on defects that genuinely prevent the goal. Return only the structured \
review output.";

/// The three output schemas, embedded at build time so they travel with the
/// binary (no cwd/path fragility). Norn treats a value starting with `{` as
/// inline JSON.
const PLAN_OUTPUT_SCHEMA: &str = include_str!("../../schemas/plan_output.json");
const DEV_OUTPUT_SCHEMA: &str = include_str!("../../schemas/dev_output.json");
const REVIEW_OUTPUT_SCHEMA: &str = include_str!("../../schemas/review_output.json");

/// The agent activity-type names this harness owns; advertised in registration
/// so the server routes them here.
pub const AGENT_ACTIVITY_TYPES: [&str; 3] = ["plan", "dev", "review"];

/// One of plan-fanout's three agent roles, resolved from the activity type.
#[derive(Debug, Clone, Copy)]
enum Role {
    Plan,
    Dev,
    Review,
}

impl Role {
    fn from_activity_type(activity_type: &str) -> Result<Self, HarnessError> {
        match activity_type {
            "plan" => Ok(Self::Plan),
            "dev" => Ok(Self::Dev),
            "review" => Ok(Self::Review),
            other => Err(HarnessError::protocol(format!(
                "plan-fanout harness cannot serve unknown agent activity type `{other}`"
            ))),
        }
    }

    fn system_prompt(self) -> &'static str {
        match self {
            Self::Plan => PLAN_SYSTEM_PROMPT,
            Self::Dev => DEV_SYSTEM_PROMPT,
            Self::Review => REVIEW_SYSTEM_PROMPT,
        }
    }

    fn output_schema(self) -> &'static str {
        match self {
            Self::Plan => PLAN_OUTPUT_SCHEMA,
            Self::Dev => DEV_OUTPUT_SCHEMA,
            Self::Review => REVIEW_OUTPUT_SCHEMA,
        }
    }
}

/// The role-dispatching composed harness. Holds only the `norn` binary path; the
/// per-run configuration is derived from each [`AgentRunSpec`].
pub struct RoleDispatchHarness {
    norn_bin: String,
}

impl RoleDispatchHarness {
    pub fn new(norn_bin: impl Into<String>) -> Self {
        Self {
            norn_bin: norn_bin.into(),
        }
    }
}

/// Derive the distinct session id for one attempt: `{workflow_id}-{hint}`, where
/// `hint` is the `session_hint` field the workflow placed in the activity input.
/// Falls back to `{workflow_id}-{activity_type}-{activity_id}` if the hint is
/// absent — still distinct per activity, never a collision.
fn session_id_for(spec: &AgentRunSpec) -> String {
    let hint = serde_json::from_slice::<serde_json::Value>(spec.input.bytes())
        .ok()
        .and_then(|value| {
            value
                .get("session_hint")
                .and_then(|hint| hint.as_str())
                .map(str::to_owned)
        });
    match hint {
        Some(hint) => format!("{}-{hint}", spec.workflow_id),
        None => format!(
            "{}-{}-{}",
            spec.workflow_id, spec.activity_type, spec.activity_id
        ),
    }
}

#[async_trait]
impl DynAgentHarness for RoleDispatchHarness {
    async fn start_dyn(
        &self,
        spec: AgentRunSpec,
    ) -> Result<Box<dyn DynAgentSession>, HarnessError> {
        let role = Role::from_activity_type(&spec.activity_type)?;
        let session_id = session_id_for(&spec);

        // A fresh NornHarness per attempt: the ONE place a concrete adapter is
        // named. The role's system prompt + output schema constrain the run, the
        // derived session id makes it a distinct resumable session, and
        // `--resume-if-exists` makes a re-dispatch after failover resume rather
        // than restart. `OPENAI_API_KEY` is stripped so Norn uses the operator's
        // ChatGPT OAuth login (a stray ambient key would take precedence and
        // fail). We then DELEGATE the real driven run to it.
        let harness = NornHarness::with_binary(&self.norn_bin)
            .with_arg("--append-system-prompt")
            .with_arg(role.system_prompt())
            .with_arg("--output-schema")
            .with_arg(role.output_schema().trim_start())
            .with_arg("--session-id")
            .with_arg(session_id)
            .with_arg("--resume-if-exists")
            .with_arg("--fast")
            .without_env("OPENAI_API_KEY");

        harness.start_dyn(spec).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aion_core::{ActivityId, ContentType, Payload, WorkflowId};

    fn spec(activity_type: &str, input: &str) -> AgentRunSpec {
        AgentRunSpec::new(
            WorkflowId::new_v4(),
            ActivityId::from_sequence_position(7),
            0,
            activity_type,
            Payload::new(ContentType::Json, input.as_bytes().to_vec()),
        )
    }

    #[test]
    fn role_resolves_from_activity_type() {
        assert!(matches!(Role::from_activity_type("plan"), Ok(Role::Plan)));
        assert!(matches!(Role::from_activity_type("dev"), Ok(Role::Dev)));
        assert!(matches!(
            Role::from_activity_type("review"),
            Ok(Role::Review)
        ));
        assert!(Role::from_activity_type("nope").is_err());
    }

    #[test]
    fn session_id_uses_the_hint_and_is_distinct_per_member() {
        let workflow_id = WorkflowId::new_v4();
        let workflow_prefix = workflow_id.to_string();
        let dev0 = AgentRunSpec::new(
            workflow_id.clone(),
            ActivityId::from_sequence_position(1),
            0,
            "dev",
            Payload::new(
                ContentType::Json,
                br#"{"session_hint":"u1-dev-r0"}"#.to_vec(),
            ),
        );
        let dev1 = AgentRunSpec::new(
            workflow_id,
            ActivityId::from_sequence_position(2),
            0,
            "dev",
            Payload::new(
                ContentType::Json,
                br#"{"session_hint":"u2-dev-r0"}"#.to_vec(),
            ),
        );
        let s0 = session_id_for(&dev0);
        let s1 = session_id_for(&dev1);
        assert!(s0.ends_with("-u1-dev-r0"));
        assert!(s1.ends_with("-u2-dev-r0"));
        assert_ne!(s0, s1, "same type, same workflow, distinct sessions");
        assert!(s0.starts_with(&workflow_prefix));
    }

    #[test]
    fn session_id_falls_back_when_hint_absent() {
        let session_id = session_id_for(&spec("review", "{}"));
        assert!(session_id.contains("-review-"));
        assert!(
            session_id.contains("activity:7"),
            "falls back to activity id: {session_id}"
        );
    }
}
