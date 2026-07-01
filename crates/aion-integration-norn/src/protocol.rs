//! Norn's on-wire JSON-RPC contract — the ONLY module in the workspace that names it.
//!
//! Everything Norn-specific about the `--protocol jsonrpc` channel — the method namespace, the
//! `initialize` capability shape, the `event/*` notification `type` labels, the `intervene/*`
//! param field names — is named here (and consumed by [`crate::translate`]). Nothing above the
//! adapter references these strings; that is the §3A.4 `no-norn-in-platform` invariant.
//!
//! The contract mirrors Norn's `norn-cli` driven-mode transport verbatim (its
//! `print::jsonrpc` / `print::intervene` modules): `initialize` advertises
//! `capabilities.interventions` (`inject_message` + `cancel`), `run/execute` returns the single
//! id-matched result Response, `event/*` notifications stream the transcript, and
//! `intervene/injectMessage` / `intervene/cancel` are the two acknowledged control requests.

/// The `initialize` handshake method.
pub const METHOD_INITIALIZE: &str = "initialize";
/// The `run/execute` method whose id-matched Response is the replay-authoritative result.
pub const METHOD_RUN_EXECUTE: &str = "run/execute";
/// The `intervene/injectMessage` control request — maps [`InjectMessage`].
///
/// [`InjectMessage`]: aion_core::InterventionKind::InjectMessage
pub const METHOD_INTERVENE_INJECT: &str = "intervene/injectMessage";
/// The `intervene/cancel` control request — maps [`Cancel`].
///
/// [`Cancel`]: aion_core::InterventionKind::Cancel
pub const METHOD_INTERVENE_CANCEL: &str = "intervene/cancel";

/// The `event/*` notification method prefix.
pub const EVENT_METHOD_PREFIX: &str = "event/";

/// The neutral capability label Norn advertises for [`InjectMessage`].
///
/// [`InjectMessage`]: aion_core::InterventionKind::InjectMessage
pub const CAPABILITY_INJECT_MESSAGE: &str = "inject_message";
/// The neutral capability label Norn advertises for [`Cancel`].
///
/// [`Cancel`]: aion_core::InterventionKind::Cancel
pub const CAPABILITY_CANCEL: &str = "cancel";
/// The neutral capability label for [`PauseResume`].
///
/// [`PauseResume`]: aion_core::InterventionKind::PauseResume
pub const CAPABILITY_PAUSE_RESUME: &str = "pause_resume";
/// The neutral capability label for [`UpdateBudget`].
///
/// [`UpdateBudget`]: aion_core::InterventionKind::UpdateBudget
pub const CAPABILITY_UPDATE_BUDGET: &str = "update_budget";
/// The neutral capability label for [`RespondToApproval`].
///
/// [`RespondToApproval`]: aion_core::InterventionKind::RespondToApproval
pub const CAPABILITY_RESPOND_TO_APPROVAL: &str = "respond_to_approval";

/// The `interventions` array key inside the `initialize` result's `capabilities` object.
pub const CAPABILITIES_KEY: &str = "capabilities";
/// The `interventions` array key inside the `capabilities` object.
pub const INTERVENTIONS_KEY: &str = "interventions";

/// The `run/execute` param key carrying the prompt (Norn also accepts `input` as an alias; the
/// adapter always sends `prompt`).
pub const PARAM_PROMPT: &str = "prompt";
/// The `intervene/injectMessage` param key carrying the message text.
pub const PARAM_TEXT: &str = "text";
/// The `intervene/injectMessage` param key carrying the priority.
pub const PARAM_PRIORITY: &str = "priority";
/// The `intervene/cancel` param key carrying the reason.
pub const PARAM_REASON: &str = "reason";

/// The `priority` value for an interrupt-priority injection (steer-now).
pub const PRIORITY_INTERRUPT: &str = "interrupt";
/// The `priority` value for a normal-priority injection (queued turn).
pub const PRIORITY_NORMAL: &str = "normal";

/// The `agent_id` field every `event/*` notification param carries (added by Norn's emitter).
pub const EVENT_AGENT_ID: &str = "agent_id";
/// The `agent_role` field every `event/*` notification param carries.
pub const EVENT_AGENT_ROLE: &str = "agent_role";
/// The `type` field every `event/*` notification param carries (Norn's native event label).
pub const EVENT_TYPE: &str = "type";
