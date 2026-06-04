//! Builder-supplied scheduler configuration for the embedded runtime.

/// Configuration used when constructing the embedded BEAM runtime.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeConfig {
    /// Optional scheduler thread count supplied by the engine builder.
    ///
    /// `None` is passed through to beamr so the embedded runtime applies its own
    /// runtime-aware default.
    pub thread_count: Option<usize>,
}

impl RuntimeConfig {
    /// Create runtime configuration from the builder-supplied scheduler count.
    #[must_use]
    pub const fn new(thread_count: Option<usize>) -> Self {
        Self { thread_count }
    }
}
