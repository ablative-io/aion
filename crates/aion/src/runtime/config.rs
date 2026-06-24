//! Builder-supplied scheduler configuration for the embedded runtime.

use std::time::Duration;

/// Configuration used when constructing the embedded BEAM runtime.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeConfig {
    /// Optional scheduler thread count supplied by the engine builder.
    ///
    /// `None` is passed through to beamr so the embedded runtime applies its own
    /// runtime-aware default.
    pub thread_count: Option<usize>,

    /// Bounded readiness and retry policy for live signal mailbox delivery.
    pub signal_delivery: SignalDeliveryConfig,

    /// Whether the durable-outbox fan-out dispatch path is enabled.
    pub outbox_enabled: bool,
}

/// Bounded signal delivery retry policy supplied by engine configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalDeliveryConfig {
    /// Maximum time to wait for a just-spawned process body to materialize.
    pub ready_timeout: Duration,

    /// Maximum number of mailbox enqueue attempts after the ready gate.
    pub max_enqueue_attempts: u32,

    /// Initial sleep between failed enqueue attempts.
    pub initial_backoff: Duration,

    /// Upper bound for exponential backoff between enqueue attempts.
    pub max_backoff: Duration,
}

impl Default for SignalDeliveryConfig {
    fn default() -> Self {
        Self::new(
            Duration::from_millis(50),
            8,
            Duration::from_millis(1),
            Duration::from_millis(8),
        )
    }
}

impl SignalDeliveryConfig {
    /// Create an explicit signal delivery policy.
    #[must_use]
    pub const fn new(
        ready_timeout: Duration,
        max_enqueue_attempts: u32,
        initial_backoff: Duration,
        max_backoff: Duration,
    ) -> Self {
        Self {
            ready_timeout,
            max_enqueue_attempts,
            initial_backoff,
            max_backoff,
        }
    }
}

impl RuntimeConfig {
    /// Create runtime configuration from the builder-supplied scheduler count.
    #[must_use]
    pub fn new(thread_count: Option<usize>) -> Self {
        Self {
            thread_count,
            signal_delivery: SignalDeliveryConfig::default(),
            outbox_enabled: false,
        }
    }

    /// Override the signal delivery retry policy.
    #[must_use]
    pub const fn with_signal_delivery(mut self, signal_delivery: SignalDeliveryConfig) -> Self {
        self.signal_delivery = signal_delivery;
        self
    }

    /// Override whether the durable-outbox fan-out dispatch path is enabled.
    #[must_use]
    pub const fn with_outbox_enabled(mut self, enabled: bool) -> Self {
        self.outbox_enabled = enabled;
        self
    }
}
