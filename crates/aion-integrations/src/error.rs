//! The harness-neutral error taxonomy for the integration seam.
//!
//! [`HarnessError`] is the single error type every [`crate::AgentHarness`] /
//! [`crate::AgentSession`] method returns. It is **harness-neutral**: no variant names a
//! concrete harness, and only the transport/protocol variants reference the notion of a wire
//! at all (as generic descriptions, never a specific protocol type). An adapter maps its own
//! failures onto these variants; callers above the adapter branch on the variant alone.

/// The neutral error taxonomy for the harness-integration seam.
///
/// Every arm is harness-neutral. [`Self::CapabilityNotSupported`] is the first-class outcome an
/// observability-only harness returns from [`crate::AgentSession::intervene`] for any command —
/// it is a legitimate, gated rejection, not an internal failure.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HarnessError {
    /// The requested intervention primitive is not in the harness's advertised capability set.
    ///
    /// This is the first-class rejection an observability-only harness (empty capability set)
    /// returns for *every* command, and the rejection any harness returns for a primitive it did
    /// not advertise. It is a normal, expected outcome of capability gating — not a fault.
    #[error("capability not supported: {primitive}")]
    CapabilityNotSupported {
        /// A neutral label naming the unsupported primitive (e.g. `"pause_resume"`).
        primitive: String,
    },
    /// The command targets a stale or unknown activity attempt and is a no-op.
    ///
    /// A command addressed to a superseded attempt (a later attempt is now live) or to a session
    /// that has already reached its terminal result is dropped without effect.
    #[error("stale target: {detail}")]
    StaleTarget {
        /// Human-readable detail describing why the target is stale.
        detail: String,
    },
    /// The underlying transport failed (spawn/connect failure, broken pipe, EOF, I/O error).
    ///
    /// Neutral: it describes *that* the transport failed and carries the detail, never *which*
    /// transport. An adapter maps its own I/O failures here.
    #[error("transport error: {detail}")]
    Transport {
        /// Human-readable description of the transport failure.
        detail: String,
    },
    /// A message was received that violates the wire protocol contract.
    ///
    /// Malformed framing, an undecodable envelope, a response that correlates to no outstanding
    /// request, or a terminal result delivered on the wrong message kind. This signals a bug in
    /// the peer or the adapter, distinct from an ordinary transport outage.
    #[error("protocol error: {detail}")]
    Protocol {
        /// Human-readable description of the protocol violation.
        detail: String,
    },
    /// The harness reported an application-level failure while running the agent.
    ///
    /// The agent ran but ended in failure (a non-success exit, an error result, a rejected run).
    /// Distinct from [`Self::Transport`] (the channel broke) and [`Self::Protocol`] (a malformed
    /// message): here the channel and framing were sound and the harness *reported* failure.
    #[error("harness reported failure: {detail}")]
    Harness {
        /// Human-readable description of the reported failure.
        detail: String,
    },
}

impl HarnessError {
    /// Builds a [`Self::CapabilityNotSupported`] naming the unsupported primitive.
    #[must_use]
    pub fn capability_not_supported(primitive: impl Into<String>) -> Self {
        Self::CapabilityNotSupported {
            primitive: primitive.into(),
        }
    }

    /// Builds a [`Self::StaleTarget`] with a detail message.
    #[must_use]
    pub fn stale_target(detail: impl Into<String>) -> Self {
        Self::StaleTarget {
            detail: detail.into(),
        }
    }

    /// Builds a [`Self::Transport`] with a detail message.
    #[must_use]
    pub fn transport(detail: impl Into<String>) -> Self {
        Self::Transport {
            detail: detail.into(),
        }
    }

    /// Builds a [`Self::Protocol`] with a detail message.
    #[must_use]
    pub fn protocol(detail: impl Into<String>) -> Self {
        Self::Protocol {
            detail: detail.into(),
        }
    }

    /// Builds a [`Self::Harness`] with a detail message.
    #[must_use]
    pub fn harness(detail: impl Into<String>) -> Self {
        Self::Harness {
            detail: detail.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HarnessError;

    fn assert_send_sync_static<T: Send + Sync + 'static>() {}

    #[test]
    fn harness_error_is_send_sync_static() {
        assert_send_sync_static::<HarnessError>();
    }

    #[test]
    fn capability_not_supported_names_the_primitive() {
        let error = HarnessError::capability_not_supported("pause_resume");
        assert_eq!(error.to_string(), "capability not supported: pause_resume");
        assert!(matches!(error, HarnessError::CapabilityNotSupported { .. }));
    }

    #[test]
    fn each_constructor_renders_its_class() {
        assert_eq!(
            HarnessError::stale_target("attempt 2 superseded").to_string(),
            "stale target: attempt 2 superseded"
        );
        assert_eq!(
            HarnessError::transport("broken pipe").to_string(),
            "transport error: broken pipe"
        );
        assert_eq!(
            HarnessError::protocol("no matching id").to_string(),
            "protocol error: no matching id"
        );
        assert_eq!(
            HarnessError::harness("exit code 1").to_string(),
            "harness reported failure: exit code 1"
        );
    }
}
