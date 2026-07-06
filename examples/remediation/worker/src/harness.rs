//! [`ProfiledNornHarness`] — the thin harness wrapper that assembles the
//! role prompt before delegating to the real [`NornHarness`].
//!
//! WHY A WRAPPER: the remediation prompt is {profile markdown, loaded at
//! startup} + {the activity's per-run context JSON}. The profile cannot ride
//! in the activity input (the workflow does not have it — it is a worker-side
//! `--profiles-dir` concern), and `NornHarness` derives its prompt from the
//! input payload alone. So the wrapper intercepts exactly one seam: it reads
//! the spec's input payload (the context JSON the workflow encoded), runs the
//! role's ONE assembly function (`crate::prompts`), and hands the inner
//! harness the same spec with its input replaced by the assembled prompt as a
//! JSON string — which `NornHarness` unwraps verbatim. Everything else
//! (driven mode, jsonrpc, `--output-schema`, `{workflow_id}` session
//! identity, env hygiene) stays the inner harness's, untouched.

use aion_integration_norn::NornHarness;
use aion_integrations::contract::AgentHarness;
use aion_integrations::{AgentRunSpec, HarnessError, Payload};
use async_trait::async_trait;
use serde_json::Value;

use crate::prompts::AssembleFn;

/// A per-role harness: the composed inner [`NornHarness`], the role's profile
/// markdown (loaded once at startup), and the role's prompt assembly
/// function.
#[derive(Clone, Debug)]
pub struct ProfiledNornHarness {
    inner: NornHarness,
    profile: String,
    assemble: AssembleFn,
}

impl ProfiledNornHarness {
    /// Wrap a composed inner harness with a role profile and its assembly
    /// function.
    #[must_use]
    pub fn new(inner: NornHarness, profile: String, assemble: AssembleFn) -> Self {
        Self {
            inner,
            profile,
            assemble,
        }
    }

    /// Assemble the prompt this harness would send for `context_json` —
    /// exposed so tests exercise the exact production assembly path.
    #[must_use]
    pub fn assembled_prompt(&self, context_json: &str) -> String {
        (self.assemble)(&self.profile, context_json)
    }
}

#[async_trait]
impl AgentHarness for ProfiledNornHarness {
    type Session = <NornHarness as AgentHarness>::Session;

    async fn start(&self, mut spec: AgentRunSpec) -> Result<Self::Session, HarnessError> {
        // The input payload is the workflow-encoded context JSON (an object;
        // for the test-author, already recommendation-free by construction).
        let context_json = std::str::from_utf8(spec.input.bytes())
            .map_err(|source| {
                HarnessError::protocol(format!("run input is not valid UTF-8: {source}"))
            })?
            .to_owned();
        let prompt = self.assembled_prompt(&context_json);
        // Re-encode as a JSON string so the inner harness's prompt derivation
        // unwraps it to the exact assembled text.
        spec.input = Payload::from_json(&Value::String(prompt)).map_err(|source| {
            HarnessError::protocol(format!("could not encode the assembled prompt: {source}"))
        })?;
        self.inner.start(spec).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::ProfiledNornHarness;
    use aion_integration_norn::NornHarness;

    #[test]
    fn the_assembled_prompt_is_the_role_function_applied_to_the_loaded_profile() {
        let harness = ProfiledNornHarness::new(
            NornHarness::new(),
            "# Verifier\nrefute with evidence".to_owned(),
            crate::prompts::verifier,
        );
        let prompt = harness.assembled_prompt("{\"diff\":\"...\"}");
        assert!(prompt.starts_with("# Verifier"));
        assert!(prompt.contains("refute with evidence"));
        assert!(prompt.contains("{\"diff\":\"...\"}"));
    }
}
