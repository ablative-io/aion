//! Norn's on-wire JSON-RPC contract — the ONLY module in the workspace that names it.
//!
//! Everything Norn-specific about the `--protocol jsonrpc` channel — the method namespace, the
//! `initialize` capability shape, the `event/*` notification `type` labels, the `intervene/*`
//! param field names — is named here (and consumed by [`crate::translate`]). Nothing above the
//! adapter references these strings; that is the §3A.4 `no-norn-in-platform` invariant.
//!
//! The contract mirrors Norn's `norn-cli` driven-mode transport verbatim (its
//! `print::jsonrpc` / `print::intervene` modules): `initialize` advertises
//! `protocol: "norn-driven/1"` plus `capabilities.interventions` (`inject_message` + `cancel`),
//! `run/execute` returns the single id-matched result Response carrying the versioned stop
//! envelope (`envelope_version` / `stop` / `output`), `event/*` notifications stream the
//! transcript, and `intervene/injectMessage` / `intervene/cancel` are the two acknowledged
//! control requests.

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

/// The `protocol` key of the `initialize` result, naming the driven-mode contract version.
pub const PROTOCOL_KEY: &str = "protocol";
/// The driven-mode protocol version this adapter speaks; the handshake gates on it.
///
/// It replaced the old `protocolVersion: "2.0"` advertisement — a missing or different value
/// means the spawned `norn` binary predates (or postdates, incompatibly) this adapter's contract.
pub const PROTOCOL_VERSION: &str = "norn-driven/1";

/// The `interventions` array key inside the `initialize` result's `capabilities` object.
pub const CAPABILITIES_KEY: &str = "capabilities";
/// The `interventions` array key inside the `capabilities` object.
pub const INTERVENTIONS_KEY: &str = "interventions";

/// The version key of the `run/execute` result envelope (`envelope_version: 1`).
pub const ENVELOPE_VERSION_KEY: &str = "envelope_version";
/// The `stop` object of the result envelope, internally tagged on [`STOP_REASON_KEY`].
pub const STOP_KEY: &str = "stop";
/// The `reason` tag inside the envelope's `stop` object.
pub const STOP_REASON_KEY: &str = "reason";
/// The `output` value of the result envelope — the run's final output.
pub const OUTPUT_KEY: &str = "output";
/// The one `stop.reason` whose envelope carries a usable `output` (every other reason is a
/// non-completion the adapter surfaces as a harness failure).
pub const STOP_REASON_COMPLETED: &str = "completed";

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
