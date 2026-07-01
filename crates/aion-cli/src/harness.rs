//! The agent-harness composition root (NOI-4b, §3A.3).
//!
//! This is the ONE place in the `aion` binary that names a concrete
//! [`AgentHarness`] adapter. Behind the default-on `norn` cargo feature it
//! composes the first-party [`NornHarness`] (`aion-integration-norn`) in, so the
//! shipped default binary drives Norn agents OUT-OF-BOX. The platform library
//! crates never name the adapter — the worker drives ANY `AgentHarness` through
//! the neutral trait (`aion_worker::spawn_agent`); this root only injects which
//! concrete adapter is compiled in.
//!
//! Mirrors the proven `liminal-transport` split exactly: the concrete adapter is
//! an OPTIONAL dependency, ON in the binary's `default = [...]`, OFF under
//! `--no-default-features` — at which point the binary carries no Norn code and
//! the `no-norn-in-platform` gate proves the library crates never did.
//!
//! [`AgentHarness`]: aion_integrations::AgentHarness
//! [`NornHarness`]: aion_integration_norn::NornHarness

/// The name of the agent harness compiled into this binary, or `None` when no
/// harness feature is enabled (`--no-default-features` without `norn`).
///
/// Reported once at server start so an operator sees whether the shipped default
/// (Norn) is in this build.
/// The harness compiled in, resolved at compile time: `Some(name)` with the
/// `norn` feature, `None` without it. A `const` (not an `if cfg!`) so the value is
/// genuinely `Option`-shaped under every configuration and carries no
/// unnecessary-wrap.
#[cfg(feature = "norn")]
const COMPILED_HARNESS: Option<&str> = Some("norn (aion-integration-norn)");
#[cfg(not(feature = "norn"))]
const COMPILED_HARNESS: Option<&str> = None;

#[must_use]
pub fn compiled_harness_name() -> Option<&'static str> {
    COMPILED_HARNESS
}

/// Announces the composed agent harness at server start.
///
/// Constructs the default harness when one is compiled in (proving the
/// composition root is live, not merely declared) and logs its name; logs that
/// no harness is compiled otherwise. Constructing a [`NornHarness`] here spawns
/// nothing — it only captures the binary path — so this is a cheap readiness
/// report, and it is the production use that makes [`default_agent_harness`]
/// reachable from the running binary rather than tests alone.
pub fn announce_composed_harness() {
    match compiled_harness_name() {
        Some(name) => {
            #[cfg(feature = "norn")]
            {
                // Build the harness the worker would drive, proving it composes.
                let _harness = default_agent_harness();
            }
            eprintln!(
                "aion server: agent harness composed at the binary root: {name}. \
                 The worker drives it through the neutral AgentHarness trait."
            );
        }
        None => eprintln!(
            "aion server: no agent harness compiled into this build \
             (--no-default-features without `norn`); agent activities are unavailable \
             until a harness is composed in."
        ),
    }
}

#[cfg(feature = "norn")]
pub use with_norn::default_agent_harness;

#[cfg(feature = "norn")]
mod with_norn {
    use aion_integration_norn::NornHarness;
    use aion_integrations::AgentHarness;

    /// The default agent harness the worker drives when the `norn` feature is on:
    /// a [`NornHarness`] invoking `norn` from `PATH` in its `--protocol jsonrpc`
    /// mode.
    ///
    /// Returned as an `impl `[`AgentHarness`] so the composition root is the only
    /// place naming the concrete type; every caller drives it through the neutral
    /// trait. The worker's `aion_worker::spawn_agent` takes it by reference.
    #[must_use]
    pub fn default_agent_harness() -> impl AgentHarness {
        NornHarness::new()
    }
}

#[cfg(all(test, feature = "norn"))]
mod tests {
    use aion_integration_norn::NornHarness;
    use aion_integrations::AgentHarness;

    /// With the default (`norn`) feature on, the composition root yields a harness
    /// the worker can drive through the neutral trait — the out-of-box wiring.
    /// Asserts its argument satisfies the neutral [`AgentHarness`] trait — the
    /// only thing the worker driver requires of the composed harness.
    fn assert_harness<H: AgentHarness>(_harness: &H) {}

    #[test]
    fn default_harness_is_available_with_the_norn_feature() {
        // Object-usable through the neutral trait. We do not spawn `norn` here (no
        // binary in the unit-test sandbox); we only prove the composition root
        // produces a value satisfying `AgentHarness`, which is what the worker
        // driver requires.
        let harness = super::default_agent_harness();
        assert_harness(&harness);
    }

    /// The composition root injects a plain default `NornHarness` — a thin
    /// injector that adds no configuration the worker path would not see.
    #[test]
    fn composition_root_injects_a_plain_norn_harness() {
        let _direct = NornHarness::new();
        let _composed = super::default_agent_harness();
    }

    /// The reported harness name is present when the feature is on.
    #[test]
    fn compiled_harness_name_reports_norn_when_on() {
        assert_eq!(
            super::compiled_harness_name(),
            Some("norn (aion-integration-norn)")
        );
    }
}
