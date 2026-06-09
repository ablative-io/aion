//! Deterministic workflow-visible time and random NIF implementations.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock, RwLock};

use aion_core::{RunId, WorkflowId};
use aion_store::EventStore;
use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary::{self, Binary};
use beamr::term::boxed;
use rand_chacha::ChaCha20Rng;
use rand_core::{Rng, SeedableRng};
use sha2::{Digest, Sha256};
use tokio::runtime::Handle;

use crate::registry::Registry;

use super::nif_context::NifContext;

const RANDOM_SEED_DOMAIN: &[u8] = b"aion.runtime.nif.determinism.rng.v1.sha256.chacha20";
const FLOAT_SCALE: f64 = 9_007_199_254_740_992.0;

thread_local! {
    static NIF_HEAP: RefCell<Vec<Box<[u64]>>> = const { RefCell::new(Vec::new()) };
}

static CONTEXT_SOURCE: OnceLock<RwLock<Arc<NifContextSource>>> = OnceLock::new();

/// Inputs required to resolve per-call workflow NIF context from a raw process id.
pub(crate) struct NifContextSource {
    registry: Arc<Registry>,
    tokio_handle: Handle,
    store: Arc<dyn EventStore>,
}

impl NifContextSource {
    /// Creates an installed deterministic-NIF context source.
    #[must_use]
    pub fn new(registry: Arc<Registry>, tokio_handle: Handle, store: Arc<dyn EventStore>) -> Self {
        Self {
            registry,
            tokio_handle,
            store,
        }
    }

    fn context_for_pid(&self, pid: u64) -> Result<NifContext, String> {
        NifContext::new_with_history_store(
            pid,
            self.registry.as_ref(),
            self.tokio_handle.clone(),
            Some(Arc::clone(&self.store)),
        )
        .map_err(|error| error.to_string())
    }
}

/// Installs the process-wide context source used by production deterministic NIFs.
pub(crate) fn install_nif_context_source(source: Arc<NifContextSource>) {
    match CONTEXT_SOURCE.get() {
        Some(installed) => match installed.write() {
            Ok(mut guard) => *guard = source,
            Err(poisoned) => *poisoned.into_inner() = source,
        },
        None => {
            if let Err(source_lock) = CONTEXT_SOURCE.set(RwLock::new(source)) {
                if let Some(installed) = CONTEXT_SOURCE.get() {
                    let source = source_lock
                        .into_inner()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    match installed.write() {
                        Ok(mut guard) => *guard = source,
                        Err(poisoned) => *poisoned.into_inner() = source,
                    }
                }
            }
        }
    }
}

fn park_heap(heap: Box<[u64]>) {
    NIF_HEAP.with_borrow_mut(|parked| parked.push(heap));
}

#[cfg(test)]
fn clear_parked_heap() {
    NIF_HEAP.with_borrow_mut(Vec::clear);
}

fn alloc_binary_term(bytes: &[u8]) -> Option<Term> {
    let word_count = 2 + binary::packed_word_count(bytes.len());
    let mut heap = vec![0_u64; word_count].into_boxed_slice();
    let term = binary::write_binary(&mut heap, bytes)?;
    park_heap(heap);
    Some(term)
}

fn alloc_tuple_term(elements: &[Term]) -> Option<Term> {
    let word_count = 1 + elements.len();
    let mut heap = vec![0_u64; word_count].into_boxed_slice();
    let term = boxed::write_tuple(&mut heap, elements)?;
    park_heap(heap);
    Some(term)
}

fn ok_result_term(value: &str) -> Option<Term> {
    let value_term = alloc_binary_term(value.as_bytes())?;
    alloc_tuple_term(&[Term::atom(Atom::OK), value_term])
}

fn error_result_term(message: &str) -> Option<Term> {
    let value_term = alloc_binary_term(message.as_bytes())?;
    alloc_tuple_term(&[Term::atom(Atom::ERROR), value_term])
}

fn decode_string_arg(term: Term) -> Result<String, String> {
    let bin = Binary::new(term).ok_or_else(|| "argument is not a binary".to_owned())?;
    String::from_utf8(bin.as_bytes().to_vec()).map_err(|_| "argument is not valid UTF-8".to_owned())
}

fn context_from_process(ctx: &ProcessContext) -> Result<NifContext, String> {
    let pid = ctx
        .pid()
        .ok_or_else(|| "determinism NIF called without a process id".to_owned())?;
    let source = CONTEXT_SOURCE
        .get()
        .ok_or_else(|| "determinism NIF context source is not installed".to_owned())?;
    let guard = source
        .read()
        .map_err(|_| "determinism NIF context source lock is poisoned".to_owned())?;
    guard.context_for_pid(pid)
}

fn error_term(message: &str) -> Term {
    match error_result_term(message) {
        Some(term) => term,
        None => Term::NIL,
    }
}

fn ok_term_or_nil(value: &str) -> Term {
    match ok_result_term(value) {
        Some(term) => term,
        None => Term::NIL,
    }
}

/// NIF backing `aion_flow_ffi:now/0`.
pub(crate) fn now_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if !args.is_empty() {
        return Ok(error_term(&format!(
            "now: expected 0 arguments, got {}",
            args.len()
        )));
    }

    match context_from_process(ctx) {
        Ok(context) => Ok(now_from_context(&context)),
        Err(error) => Ok(error_term(&error)),
    }
}

/// NIF backing `aion_flow_ffi:random/0`.
pub(crate) fn random_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if !args.is_empty() {
        return Ok(error_term(&format!(
            "random: expected 0 arguments, got {}",
            args.len()
        )));
    }

    match context_from_process(ctx) {
        Ok(context) => Ok(random_from_context(&context)),
        Err(error) => Ok(error_term(&error)),
    }
}

/// NIF backing `aion_flow_ffi:random_int/2`.
pub(crate) fn random_int_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 2 {
        return Ok(error_term(&format!(
            "random_int: expected 2 arguments, got {}",
            args.len()
        )));
    }

    let min = match parse_i64_arg(args[0], "random_int min") {
        Ok(value) => value,
        Err(message) => return Ok(error_term(&message)),
    };
    let max = match parse_i64_arg(args[1], "random_int max") {
        Ok(value) => value,
        Err(message) => return Ok(error_term(&message)),
    };
    if min > max {
        return Ok(error_term(
            "Invalid deterministic random_int range: min is greater than max",
        ));
    }

    match context_from_process(ctx) {
        Ok(context) => Ok(random_int_from_context(&context, min, max)),
        Err(error) => Ok(error_term(&error)),
    }
}

fn parse_i64_arg(term: Term, label: &str) -> Result<i64, String> {
    let text = decode_string_arg(term).map_err(|error| format!("{label}: {error}"))?;
    text.parse::<i64>()
        .map_err(|_| format!("{label}: argument is not a valid i64"))
}

fn now_from_context(context: &NifContext) -> Term {
    let Some(recorded_at) = context.last_recorded_at() else {
        return error_term("now: workflow history is empty");
    };
    ok_term_or_nil(&recorded_at.timestamp_millis().to_string())
}

fn random_from_context(context: &NifContext) -> Term {
    let sequence = context.next_deterministic_sequence();
    let value = deterministic_float(context.workflow_id(), context.run_id(), sequence);
    ok_term_or_nil(&value.to_string())
}

fn random_int_from_context(context: &NifContext, min: i64, max: i64) -> Term {
    let sequence = context.next_deterministic_sequence();
    let value = deterministic_i64(context.workflow_id(), context.run_id(), sequence, min, max);
    ok_term_or_nil(&value.to_string())
}

fn deterministic_u64(workflow_id: &WorkflowId, run_id: &RunId, sequence: u64) -> u64 {
    let mut rng = ChaCha20Rng::from_seed(seed_from_ids_and_sequence(workflow_id, run_id, sequence));
    rng.next_u64()
}

fn deterministic_float(workflow_id: &WorkflowId, run_id: &RunId, sequence: u64) -> f64 {
    let random = deterministic_u64(workflow_id, run_id, sequence) >> 11;
    let Ok(high) = u32::try_from(random >> 32) else {
        return 0.0;
    };
    let Ok(low) = u32::try_from(random & u64::from(u32::MAX)) else {
        return 0.0;
    };
    (f64::from(high) * 4_294_967_296.0 + f64::from(low)) / FLOAT_SCALE
}

fn deterministic_i64(
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
    use aion_store::{EventStore, InMemoryStore};
    use beamr::atom::Atom;
    use beamr::native::ProcessContext;
    use beamr::term::Term;
    use beamr::term::binary::Binary;
    use beamr::term::boxed::Tuple;
    use chrono::{TimeZone, Utc};
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        NifContextSource, clear_parked_heap, deterministic_float, deterministic_i64,
        install_nif_context_source, now_from_context, now_impl, random_from_context,
        random_int_from_context,
    };
    use crate::durability::Recorder;
    use crate::registry::{
        CompletionNotifier, HandleResidency, Registry, WorkflowHandle, WorkflowHandleParts,
    };
    use crate::runtime::nif_context::NifContext;

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
            runtime.block_on(store.append(&workflow_id, history, 0))?;
        }
        let head = u64::try_from(history.len())?;
        let recorder = Recorder::resume_at(workflow_id.clone(), Arc::clone(&store), head);
        let handle = WorkflowHandle::new(WorkflowHandleParts {
            workflow_id: workflow_id.clone(),
            run_id: run_id.clone(),
            pid,
            workflow_type: "checkout".to_owned(),
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
        let bin = Binary::new(value).ok_or("value should be a binary")?;
        let text = String::from_utf8(bin.as_bytes().to_vec())?;
        Ok((tag_name.to_owned(), text))
    }

    #[test]
    fn now_returns_last_recorded_event_timestamp_millis() -> TestResult {
        clear_parked_heap();
        let runtime = tokio::runtime::Runtime::new()?;
        let workflow_id = workflow_id();
        let history = vec![
            started_event(&workflow_id, 1, 1_700_000_000_123)?,
            completed_event(&workflow_id, 2, 1_700_000_999_456)?,
        ];
        let context = context_with_history(&runtime, 41, workflow_id, run_id(), &history)?;

        let (tag, value) = decode_result_tuple(now_from_context(&context))?;

        assert_eq!(tag, "ok");
        assert_eq!(value, "1700000999456");
        Ok(())
    }

    #[test]
    fn install_context_source_replaces_stale_engine_context() -> TestResult {
        clear_parked_heap();
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
        install_nif_context_source(Arc::new(NifContextSource::new(
            Arc::clone(&first.registry),
            runtime.handle().clone(),
            Arc::clone(&first.store),
        )));

        let second_history = vec![completed_event(&workflow_id, 1, 1_700_000_999_456)?];
        let second =
            context_fixture_with_history(&runtime, 52, workflow_id, run_id(), &second_history)?;
        install_nif_context_source(Arc::new(NifContextSource::new(
            Arc::clone(&second.registry),
            runtime.handle().clone(),
            Arc::clone(&second.store),
        )));
        let mut ctx = ProcessContext::new();
        ctx.set_pid(Some(52));

        let result = now_impl(&[], &mut ctx).map_err(|_| "now should return Ok at beamr level")?;
        let (tag, value) = decode_result_tuple(result)?;

        assert_eq!(tag, "ok");
        assert_eq!(value, "1700000999456");
        Ok(())
    }

    #[test]
    fn random_is_stable_for_same_ids_and_sequence() {
        let workflow_id = workflow_id();
        let run_id = run_id();
        let first = deterministic_float(&workflow_id, &run_id, 7);
        let second = deterministic_float(&workflow_id, &run_id, 7);
        let different = deterministic_float(&workflow_id, &run_id, 8);

        assert_eq!(first, second);
        assert_ne!(first, different);
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
        clear_parked_heap();
        let min = super::alloc_binary_term(b"10").ok_or("failed to allocate min")?;
        let max = super::alloc_binary_term(b"1").ok_or("failed to allocate max")?;
        let mut ctx = ProcessContext::new();

        let result = super::random_int_impl(&[min, max], &mut ctx);
        let term = result.map_err(|_| "random_int should return Ok at the beamr level")?;
        let (tag, message) = decode_result_tuple(term)?;

        assert_eq!(tag, "error");
        assert!(message.contains("min is greater than max"));
        Ok(())
    }

    #[test]
    fn replay_sequence_matches_recorded_history_and_seeded_positions() -> TestResult {
        clear_parked_heap();
        let runtime = tokio::runtime::Runtime::new()?;
        let workflow_id = workflow_id();
        let run_id = run_id();
        let history = vec![
            started_event(&workflow_id, 1, 1_700_000_000_000)?,
            completed_event(&workflow_id, 2, 1_700_000_010_000)?,
        ];
        let first =
            context_with_history(&runtime, 43, workflow_id.clone(), run_id.clone(), &history)?;
        let first_values = vec![
            decode_result_tuple(now_from_context(&first))?.1,
            decode_result_tuple(random_from_context(&first))?.1,
            decode_result_tuple(random_int_from_context(&first, 1, 100))?.1,
        ];

        let replay = context_with_history(&runtime, 44, workflow_id, run_id, &history)?;
        let replay_values = vec![
            decode_result_tuple(now_from_context(&replay))?.1,
            decode_result_tuple(random_from_context(&replay))?.1,
            decode_result_tuple(random_int_from_context(&replay, 1, 100))?.1,
        ];

        assert_eq!(first_values, replay_values);
        Ok(())
    }
}
