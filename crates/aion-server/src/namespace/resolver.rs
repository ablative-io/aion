//! Namespace resolver type wired into shared state.

use crate::config::{NamespaceConfig, NamespaceMode};

/// Placeholder namespace resolver boundary for later authorization logic.
#[derive(Clone)]
pub struct NamespaceResolver {
    mode: NamespaceMode,
}

impl NamespaceResolver {
    /// Build a resolver from operator-supplied namespace configuration.
    ///
    /// Authorization and scoping behavior is implemented by the next brief; this
    /// type exists now so all transports receive the same boundary object.
    #[must_use]
    pub fn from_config(config: NamespaceConfig) -> Self {
        Self { mode: config.mode }
    }

    /// Inspect the configured namespace mode.
    #[must_use]
    pub const fn mode(&self) -> &NamespaceMode {
        &self.mode
    }
}
