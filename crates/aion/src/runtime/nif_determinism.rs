//! Deterministic workflow-visible time and random NIF implementations.

use std::sync::Arc;

use aion_core::{RunId, WorkflowId};
use aion_store::EventStore;
use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary_ref::BinaryRef;
use rand_chacha::ChaCha20Rng;
use rand_core::{Rng, SeedableRng};
use sha2::{Digest, Sha256};
use tokio::runtime::Handle;

use crate::registry::Registry;

use super::nif_context::NifContext;
use super::nif_state::EngineNifState;

const RANDOM_SEED_DOMAIN: &[u8] = b"aion.runtime.nif.determinism.rng.v1.sha256.chacha20";
const FLOAT_SCALE: f64 = 9_007_199_254_740_992.0;

/// Inputs required to resolve per-call workflow NIF context from a raw process id.
pub(crate) struct NifContextSource {
    registry: Arc<Registry>,
    tokio_handle: Handle,
    store: Arc<dyn EventStore>,
    /// Builder-supplied bound for the registry-registration birth wait.
    birth_wait: crate::runtime::SignalDeliveryConfig,
}

impl NifContextSource {
    /// Creates an installed deterministic-NIF context source.
    #[must_use]
    pub fn new(
        registry: Arc<Registry>,
        tokio_handle: Handle,
        store: Arc<dyn EventStore>,
        birth_wait: crate::runtime::SignalDeliveryConfig,
    ) -> Self {
        Self {
            registry,
            tokio_handle,
            store,
            birth_wait,
        }
    }

    fn context_for_pid(&self, pid: u64) -> Result<NifContext, String> {
        NifContext::new_with_history_store(
            pid,
            self.registry.as_ref(),
            self.tokio_handle.clone(),
            Some(Arc::clone(&self.store)),
            self.birth_wait,
        )
        .map_err(|error| error.to_string())
    }
}

/// Installs the engine-scoped context source used by production deterministic NIFs.
pub(crate) fn install_nif_context_source(state: &EngineNifState, source: Arc<NifContextSource>) {
    match state.context_source.write() {
        Ok(mut guard) => *guard = Some(source),
        Err(poisoned) => *poisoned.into_inner() = Some(source),
    }
}

/// Build `{ok, <<value>>}` on the calling process heap.
///
/// Result terms are allocated through the [`ProcessContext`] allocators:
/// attached (normal-scheduler) calls get GC-traced process-heap terms, and
/// detached (dirty) calls get owned blocks the dirty-result bridge copies
/// onto the process heap. Nothing is parked in thread-locals — beamr's
/// moving GC never traces out-of-heap pointers, so a parked heap either
/// leaks for the scheduler thread's lifetime or dangles once cleared while
/// workflow code still references the term (N-6).
///
/// Allocation may collect on attached calls: decode every argument `Term`
/// before the first result allocation.
fn ok_result_term(ctx: &mut ProcessContext, value: &str) -> Option<Term> {
    let value_term = ctx.alloc_binary(value.as_bytes()).ok()?;
    ctx.alloc_tuple(&[Term::atom(Atom::OK), value_term]).ok()
}

/// Build `{error, <<message>>}` on the calling process heap (see
/// [`ok_result_term`] for the allocation contract).
fn error_result_term(ctx: &mut ProcessContext, message: &str) -> Option<Term> {
    let value_term = ctx.alloc_binary(message.as_bytes()).ok()?;
    ctx.alloc_tuple(&[Term::atom(Atom::ERROR), value_term]).ok()
}

fn decode_string_arg(term: Term) -> Result<String, String> {
    let bin = BinaryRef::new(term).ok_or_else(|| "argument is not a binary".to_owned())?;
    String::from_utf8(bin.as_bytes().to_vec()).map_err(|_| "argument is not valid UTF-8".to_owned())
}

fn context_from_process(ctx: &ProcessContext) -> Result<NifContext, String> {
    let pid = ctx
        .pid()
        .ok_or_else(|| "determinism NIF called without a process id".to_owned())?;
    let state = super::nif_state::engine_nif_state(ctx)?;
    let guard = state
        .context_source
        .read()
        .map_err(|_| "determinism NIF context source lock is poisoned".to_owned())?;
    let source = guard
        .as_ref()
        .ok_or_else(|| "determinism NIF context source is not installed".to_owned())?;
    source.context_for_pid(pid)
}

fn error_term(ctx: &mut ProcessContext, message: &str) -> Term {
    error_result_term(ctx, message).unwrap_or(Term::NIL)
}

fn ok_term_or_nil(ctx: &mut ProcessContext, value: &str) -> Term {
    ok_result_term(ctx, value).unwrap_or(Term::NIL)
}

/// NIF backing `aion_flow_ffi:workflow_id/0`.
pub(crate) fn workflow_id_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if !args.is_empty() {
        return Ok(error_term(
            ctx,
            &format!("workflow_id: expected 0 arguments, got {}", args.len()),
        ));
    }

    match context_from_process(ctx) {
        Ok(context) => Ok(ok_term_or_nil(ctx, &context.workflow_id().to_string())),
        Err(error) => Ok(error_term(ctx, &error)),
    }
}

/// NIF backing `aion_flow_ffi:now/0`.
pub(crate) fn now_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if !args.is_empty() {
        return Ok(error_term(
            ctx,
            &format!("now: expected 0 arguments, got {}", args.len()),
        ));
    }

    match context_from_process(ctx) {
        Ok(context) => Ok(now_from_context(ctx, &context)),
        Err(error) => Ok(error_term(ctx, &error)),
    }
}

/// NIF backing `aion_flow_ffi:random/0`.
pub(crate) fn random_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if !args.is_empty() {
        return Ok(error_term(
            ctx,
            &format!("random: expected 0 arguments, got {}", args.len()),
        ));
    }

    match context_from_process(ctx) {
        Ok(context) => Ok(random_from_context(ctx, &context)),
        Err(error) => Ok(error_term(ctx, &error)),
    }
}

/// NIF backing `aion_flow_ffi:random_int/2`.
pub(crate) fn random_int_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 2 {
        return Ok(error_term(
            ctx,
            &format!("random_int: expected 2 arguments, got {}", args.len()),
        ));
    }

    let min = match parse_i64_arg(args[0], "random_int min") {
        Ok(value) => value,
        Err(message) => return Ok(error_term(ctx, &message)),
    };
    let max = match parse_i64_arg(args[1], "random_int max") {
        Ok(value) => value,
        Err(message) => return Ok(error_term(ctx, &message)),
    };
    if min > max {
        return Ok(error_term(
            ctx,
            "Invalid deterministic random_int range: min is greater than max",
        ));
    }

    match context_from_process(ctx) {
        Ok(context) => Ok(random_int_from_context(ctx, &context, min, max)),
        Err(error) => Ok(error_term(ctx, &error)),
    }
}

fn parse_i64_arg(term: Term, label: &str) -> Result<i64, String> {
    let text = decode_string_arg(term).map_err(|error| format!("{label}: {error}"))?;
    text.parse::<i64>()
        .map_err(|_| format!("{label}: argument is not a valid i64"))
}

fn now_from_context(ctx: &mut ProcessContext, context: &NifContext) -> Term {
    let Some(recorded_at) = context.last_recorded_at() else {
        return error_term(ctx, "now: workflow history is empty");
    };
    ok_term_or_nil(ctx, &recorded_at.timestamp_millis().to_string())
}

fn random_from_context(ctx: &mut ProcessContext, context: &NifContext) -> Term {
    let sequence = context.next_deterministic_sequence();
    let value = deterministic_float(context.workflow_id(), context.run_id(), sequence);
    ok_term_or_nil(ctx, &value.to_string())
}

fn random_int_from_context(
    ctx: &mut ProcessContext,
    context: &NifContext,
    min: i64,
    max: i64,
) -> Term {
    let sequence = context.next_deterministic_sequence();
    let value = deterministic_i64(context.workflow_id(), context.run_id(), sequence, min, max);
    ok_term_or_nil(ctx, &value.to_string())
}

fn deterministic_u64(workflow_id: &WorkflowId, run_id: &RunId, sequence: u64) -> u64 {
    let mut rng = ChaCha20Rng::from_seed(seed_from_ids_and_sequence(workflow_id, run_id, sequence));
    rng.next_u64()
}

/// The deterministic `f64` in `[0.0, 1.0)` the production `workflow.random()`
/// path serves at NIF call ordinal `sequence` for `(workflow_id, run_id)`.
///
/// This is the single production random formula: the `random()` NIF
/// ([`random_from_context`]) calls exactly this function with the sequence the
/// handle hands out per call. It is exposed at crate visibility so the
/// time-travel inspection lens computes the *same* value the running workflow
/// received at a given draw ordinal, rather than reimplementing the formula or
/// drawing from an unrelated stream (WA-004).
pub(crate) fn deterministic_float(workflow_id: &WorkflowId, run_id: &RunId, sequence: u64) -> f64 {
    let random = deterministic_u64(workflow_id, run_id, sequence) >> 11;
    let Ok(high) = u32::try_from(random >> 32) else {
        return 0.0;
    };
    let Ok(low) = u32::try_from(random & u64::from(u32::MAX)) else {
        return 0.0;
    };
    (f64::from(high) * 4_294_967_296.0 + f64::from(low)) / FLOAT_SCALE
}

/// The deterministic `i64` in `[min, max]` the production
/// `workflow.random_int(min, max)` path serves at NIF call ordinal `sequence`
/// for `(workflow_id, run_id)`.
///
/// This is the single production bounded-random formula, exposed at crate
/// visibility for the same reason as [`deterministic_float`]: the inspection
/// lens computes the exact value the running workflow received at a draw ordinal
/// (WA-004). The caller guarantees `min <= max`; an inverted range never reaches
/// here (the NIF rejects it loudly).
pub(crate) fn deterministic_i64(
    workflow_id: &WorkflowId,
    run_id: &RunId,
    sequence: u64,
    min: i64,
    max: i64,
) -> i64 {
    let span = i128::from(max) - i128::from(min);
    let width = u128::try_from(span).map_or(1, |value| value.saturating_add(1));
    let offset = uniform_u128(width, workflow_id, run_id, sequence);
    let offset = i128::try_from(offset).unwrap_or(i128::MAX);
    let value = i128::from(min) + offset;
    match i64::try_from(value) {
        Ok(value) => value,
        Err(_) if value.is_negative() => i64::MIN,
        Err(_) => i64::MAX,
    }
}

fn uniform_u128(width: u128, workflow_id: &WorkflowId, run_id: &RunId, sequence: u64) -> u128 {
    let mut counter = 0_u64;
    loop {
        let candidate = deterministic_u128(workflow_id, run_id, sequence, counter);
        let zone = u128::MAX - (u128::MAX % width);
        if candidate < zone {
            return candidate % width;
        }
        counter = counter.saturating_add(1);
    }
}

fn deterministic_u128(
    workflow_id: &WorkflowId,
    run_id: &RunId,
    sequence: u64,
    counter: u64,
) -> u128 {
    let mut hasher = Sha256::new();
    hasher.update(RANDOM_SEED_DOMAIN);
    hasher.update(workflow_id.as_uuid().as_bytes());
    hasher.update(run_id.as_uuid().as_bytes());
    hasher.update(sequence.to_be_bytes());
    hasher.update(counter.to_be_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    u128::from_be_bytes(bytes)
}

fn seed_from_ids_and_sequence(workflow_id: &WorkflowId, run_id: &RunId, sequence: u64) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(RANDOM_SEED_DOMAIN);
    hasher.update(workflow_id.as_uuid().as_bytes());
    hasher.update(run_id.as_uuid().as_bytes());
    hasher.update(sequence.to_be_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aion_core::{Event, EventEnvelope, Payload, WorkflowStatus};
    use aion_package::ContentHash;
    use aion_store::{EventStore, InMemoryStore, WriteToken};
    use beamr::atom::Atom;
    use beamr::native::ProcessContext;
    use beamr::term::Term;
    use beamr::term::binary_ref::BinaryRef;
    use beamr::term::boxed::Tuple;
    use chrono::{TimeZone, Utc};
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        NifContextSource, deterministic_float, deterministic_i64, install_nif_context_source,
        now_from_context, now_impl, random_from_context, random_int_from_context,
    };
    use crate::durability::Recorder;
    use crate::registry::{
        CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
    };
    use crate::runtime::nif_context::NifContext;
    use crate::runtime::nif_state::EngineNifState;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn hash() -> ContentHash {
        ContentHash::from_bytes([9; 32])
    }

    fn workflow_id() -> aion_core::WorkflowId {
        aion_core::WorkflowId::new(Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888))
    }

    fn run_id() -> aion_core::RunId {
        aion_core::RunId::new(Uuid::from_u128(0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0000))
    }

    fn payload(label: &str) -> TestResult<Payload> {
        Ok(Payload::from_json(&json!({ "label": label }))?)
    }

    fn envelope(
        workflow_id: &aion_core::WorkflowId,
        seq: u64,
        millis: i64,
    ) -> TestResult<EventEnvelope> {
        let recorded_at = Utc
            .timestamp_millis_opt(millis)
            .single()
            .ok_or_else(|| format!("invalid fixed timestamp {millis}"))?;
        Ok(EventEnvelope {
            seq,
            recorded_at,
            workflow_id: workflow_id.clone(),
        })
    }

    fn started_event(
        workflow_id: &aion_core::WorkflowId,
        seq: u64,
        millis: i64,
    ) -> TestResult<Event> {
        Ok(Event::WorkflowStarted {
            envelope: envelope(workflow_id, seq, millis)?,
            workflow_type: "checkout".to_owned(),
            input: payload("input")?,
            run_id: run_id(),
            parent_run_id: None,
            package_version: aion_core::PackageVersion::new("a".repeat(64)),
        })
    }

    fn completed_event(
        workflow_id: &aion_core::WorkflowId,
        seq: u64,
        millis: i64,
    ) -> TestResult<Event> {
        Ok(Event::WorkflowCompleted {
            envelope: envelope(workflow_id, seq, millis)?,
            result: payload("done")?,
        })
    }

    struct ContextFixture {
        registry: Arc<Registry>,
        store: Arc<dyn EventStore>,
        context: NifContext,
    }

    fn context_fixture_with_history(
        runtime: &tokio::runtime::Runtime,
        pid: u64,
        workflow_id: aion_core::WorkflowId,
        run_id: aion_core::RunId,
        history: &[Event],
    ) -> TestResult<ContextFixture> {
        let registry = Arc::new(Registry::default());
        let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
        if !history.is_empty() {
            runtime.block_on(store.append(WriteToken::recorder(), &workflow_id, history, 0))?;
        }
        let head = u64::try_from(history.len())?;
        let recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(&store), head);
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid,
            workflow_type: "checkout".to_owned(),
            namespace: String::from("default"),
            loaded_version: hash(),
            cached_status: WorkflowStatus::Running,
            residency: HandleResidency::Resident,
            recorder,
            completion: CompletionNotifier::new(),
        });
        registry.insert((workflow_id, run_id), handle)?;
        let context = NifContext::new_with_history_store(
            pid,
            registry.as_ref(),
            runtime.handle().clone(),
            Some(Arc::clone(&store)),
            crate::runtime::SignalDeliveryConfig::default(),
        )?;
        Ok(ContextFixture {
            registry,
            store,
            context,
        })
    }

    fn context_with_history(
        runtime: &tokio::runtime::Runtime,
        pid: u64,
        workflow_id: aion_core::WorkflowId,
        run_id: aion_core::RunId,
        history: &[Event],
    ) -> TestResult<NifContext> {
        Ok(context_fixture_with_history(runtime, pid, workflow_id, run_id, history)?.context)
    }

    fn decode_result_tuple(term: Term) -> TestResult<(String, String)> {
        let tuple = Tuple::new(term).ok_or("result should be a tuple")?;
        let tag = tuple.get(0).ok_or("missing tag element")?;
        let value = tuple.get(1).ok_or("missing value element")?;
        let tag_name = if tag == Term::atom(Atom::OK) {
            "ok"
        } else {
            "error"
        };
        let bin = BinaryRef::new(value).ok_or("value should be a binary")?;
        let text = String::from_utf8(bin.as_bytes().to_vec())?;
        Ok((tag_name.to_owned(), text))
    }

    #[test]
    fn now_returns_last_recorded_event_timestamp_millis() -> TestResult {
        let runtime = tokio::runtime::Runtime::new()?;
        let workflow_id = workflow_id();
        let history = vec![
            started_event(&workflow_id, 1, 1_700_000_000_123)?,
            completed_event(&workflow_id, 2, 1_700_000_999_456)?,
        ];
        let context = context_with_history(&runtime, 41, workflow_id, run_id(), &history)?;

        // Detached contexts allocate owned blocks that live as long as the
        // ProcessContext itself.
        let mut ctx = ProcessContext::new();
        let (tag, value) = decode_result_tuple(now_from_context(&mut ctx, &context))?;

        assert_eq!(tag, "ok");
        assert_eq!(value, "1700000999456");
        Ok(())
    }

    #[test]
    fn each_engine_state_resolves_its_own_context_source() -> TestResult {
        let runtime = tokio::runtime::Runtime::new()?;
        let workflow_id = workflow_id();
        let first_history = vec![started_event(&workflow_id, 1, 1_700_000_000_123)?];
        let first = context_fixture_with_history(
            &runtime,
            51,
            workflow_id.clone(),
            run_id(),
            &first_history,
        )?;
        let first_state = Arc::new(EngineNifState::default());
        install_nif_context_source(
            &first_state,
            Arc::new(NifContextSource::new(
                Arc::clone(&first.registry),
                runtime.handle().clone(),
                Arc::clone(&first.store),
                crate::runtime::SignalDeliveryConfig::default(),
            )),
        );

        let second_history = vec![
            started_event(&workflow_id, 1, 1_700_000_500_000)?,
            completed_event(&workflow_id, 2, 1_700_000_999_456)?,
        ];
        let second =
            context_fixture_with_history(&runtime, 52, workflow_id, run_id(), &second_history)?;
        let second_state = Arc::new(EngineNifState::default());
        install_nif_context_source(
            &second_state,
            Arc::new(NifContextSource::new(
                Arc::clone(&second.registry),
                runtime.handle().clone(),
                Arc::clone(&second.store),
                crate::runtime::SignalDeliveryConfig::default(),
            )),
        );

        // Both engines are live at once; each call resolves only against the
        // state carried by its own runtime, never the other's.
        let mut ctx = ProcessContext::new();
        ctx.set_pid(Some(52));
        ctx.set_nif_private_data(Some(second_state));
        let result = now_impl(&[], &mut ctx).map_err(|_| "now should return Ok at beamr level")?;
        let (tag, value) = decode_result_tuple(result)?;
        assert_eq!(tag, "ok");
        assert_eq!(value, "1700000999456");

        let mut first_ctx = ProcessContext::new();
        first_ctx.set_pid(Some(51));
        first_ctx.set_nif_private_data(Some(first_state));
        let result =
            now_impl(&[], &mut first_ctx).map_err(|_| "now should return Ok at beamr level")?;
        let (tag, value) = decode_result_tuple(result)?;
        assert_eq!(tag, "ok");
        assert_eq!(value, "1700000000123");
        Ok(())
    }

    #[test]
    fn random_is_stable_for_same_ids_and_sequence() {
        let workflow_id = workflow_id();
        let run_id = run_id();
        let first = deterministic_float(&workflow_id, &run_id, 7);
        let second = deterministic_float(&workflow_id, &run_id, 7);
        let different = deterministic_float(&workflow_id, &run_id, 8);

        assert!((first - second).abs() < f64::EPSILON);
        assert!((first - different).abs() > f64::EPSILON);
        assert!((0.0..1.0).contains(&first));
    }

    #[test]
    fn random_int_is_stable_uniform_and_validates_ranges() {
        let workflow_id = workflow_id();
        let run_id = run_id();
        let first = deterministic_i64(&workflow_id, &run_id, 11, 1, 100);
        let second = deterministic_i64(&workflow_id, &run_id, 11, 1, 100);
        let different = deterministic_i64(&workflow_id, &run_id, 12, 1, 100);
        let negative = deterministic_i64(&workflow_id, &run_id, 13, -50, -10);
        let fixed = deterministic_i64(&workflow_id, &run_id, 14, 5, 5);

        assert_eq!(first, second);
        assert_ne!(first, different);
        assert!((1..=100).contains(&first));
        assert!((-50..=-10).contains(&negative));
        assert_eq!(fixed, 5);
    }

    #[test]
    fn random_int_context_returns_error_for_invalid_range() -> TestResult {
        let mut ctx = ProcessContext::new();
        let min = ctx
            .alloc_binary(b"10")
            .map_err(|_| "failed to allocate min")?;
        let max = ctx
            .alloc_binary(b"1")
            .map_err(|_| "failed to allocate max")?;

        let result = super::random_int_impl(&[min, max], &mut ctx);
        let term = result.map_err(|_| "random_int should return Ok at the beamr level")?;
        let (tag, message) = decode_result_tuple(term)?;

        assert_eq!(tag, "error");
        assert!(message.contains("min is greater than max"));
        Ok(())
    }

    #[test]
    fn replay_sequence_matches_recorded_history_and_seeded_positions() -> TestResult {
        let runtime = tokio::runtime::Runtime::new()?;
        let workflow_id = workflow_id();
        let run_id = run_id();
        let history = vec![
            started_event(&workflow_id, 1, 1_700_000_000_000)?,
            completed_event(&workflow_id, 2, 1_700_000_010_000)?,
        ];
        let first =
            context_with_history(&runtime, 43, workflow_id.clone(), run_id.clone(), &history)?;
        let mut ctx = ProcessContext::new();
        let first_values = vec![
            decode_result_tuple(now_from_context(&mut ctx, &first))?.1,
            decode_result_tuple(random_from_context(&mut ctx, &first))?.1,
            decode_result_tuple(random_int_from_context(&mut ctx, &first, 1, 100))?.1,
        ];

        let replay = context_with_history(&runtime, 44, workflow_id, run_id, &history)?;
        let replay_values = vec![
            decode_result_tuple(now_from_context(&mut ctx, &replay))?.1,
            decode_result_tuple(random_from_context(&mut ctx, &replay))?.1,
            decode_result_tuple(random_int_from_context(&mut ctx, &replay, 1, 100))?.1,
        ];

        assert_eq!(first_values, replay_values);
        Ok(())
    }
}
