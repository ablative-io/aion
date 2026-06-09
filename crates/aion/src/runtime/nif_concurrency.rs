//! Durable concurrency NIF implementations over activity dispatch.

use std::cell::RefCell;

use aion_core::{ActivityError, ActivityErrorKind, ActivityId, ContentType, Payload};
use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary::{self, Binary};
use beamr::term::boxed::{self, Cons};
use chrono::Utc;
use serde::Deserialize;

use crate::activity::bridge::{ActivityDispatcher, activity_dispatcher};
use crate::durability::{Command, CorrelationKey, Resolution, ResolveOutcome};
use crate::runtime::nif_activity::runtime_context;
use crate::runtime::nif_context::{NifContext, NifContextError};

thread_local! {
    static CONCURRENCY_NIF_HEAP: RefCell<Vec<Box<[u64]>>> = const { RefCell::new(Vec::new()) };
}

#[derive(Deserialize)]
struct ActivitySpec {
    name: String,
    input: String,
    config: String,
}

enum ActivityRunResult {
    Completed(String),
    Failed(String),
}

fn park_heap(heap: Box<[u64]>) {
    CONCURRENCY_NIF_HEAP.with_borrow_mut(|parked| parked.push(heap));
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

fn tagged_result_term(tag: Atom, bytes: &[u8]) -> Option<Term> {
    let value = alloc_binary_term(bytes)?;
    alloc_tuple_term(&[Term::atom(tag), value])
}

fn ok_result_term(bytes: &[u8]) -> Option<Term> {
    tagged_result_term(Atom::OK, bytes)
}

fn error_result_term(message: &str) -> Option<Term> {
    tagged_result_term(Atom::ERROR, message.as_bytes())
}

fn context_error_term(error: &NifContextError) -> Term {
    match error.to_error_term() {
        Ok(term) => term,
        Err(_) => Term::NIL,
    }
}

fn decode_string_arg(term: Term) -> Result<String, String> {
    let bin = Binary::new(term).ok_or_else(|| "argument is not a binary".to_owned())?;
    String::from_utf8(bin.as_bytes().to_vec()).map_err(|_| "argument is not valid UTF-8".to_owned())
}

fn decode_spec_list(term: Term, label: &str) -> Result<Vec<ActivitySpec>, Term> {
    let mut specs = Vec::new();
    let mut tail = term;
    while !tail.is_nil() {
        let cons = Cons::new(tail).ok_or_else(|| {
            error_result_term(&format!(
                "{label}: activities argument is not a proper list"
            ))
            .unwrap_or(Term::NIL)
        })?;
        let encoded = decode_string_arg(cons.head()).map_err(|error| {
            error_result_term(&format!("{label}: activity spec: {error}")).unwrap_or(Term::NIL)
        })?;
        let spec = serde_json::from_str(&encoded).map_err(|error| {
            error_result_term(&format!("{label}: invalid activity spec JSON: {error}"))
                .unwrap_or(Term::NIL)
        })?;
        specs.push(spec);
        tail = cons.tail();
    }
    Ok(specs)
}

fn json_payload(text: &str, label: &str) -> Result<Payload, Term> {
    let value = serde_json::from_str(text).map_err(|error| {
        error_result_term(&format!("{label}: invalid JSON payload: {error}")).unwrap_or(Term::NIL)
    })?;
    Payload::from_json(&value)
        .map_err(|error| error_result_term(&format!("{label}: {error}")).unwrap_or(Term::NIL))
}

fn result_payload(result: &str) -> Payload {
    Payload::new(ContentType::Json, result.as_bytes().to_vec())
}

fn activity_error(reason: String) -> ActivityError {
    ActivityError {
        kind: ActivityErrorKind::Terminal,
        message: reason,
        details: None,
    }
}

fn record_started(
    context: &NifContext,
    activity_id: ActivityId,
    activity_type: String,
    input: Payload,
) -> Result<(), Term> {
    context
        .record_activity_scheduled_started(Utc::now(), activity_id, activity_type, input)
        .map_err(|error| context_error_term(&error))
}

fn record_completed(
    context: &NifContext,
    activity_id: ActivityId,
    result: Payload,
) -> Result<(), Term> {
    context
        .record_activity_completed(Utc::now(), activity_id, result)
        .map_err(|error| context_error_term(&error))
}

fn record_failed(
    context: &NifContext,
    activity_id: ActivityId,
    error: ActivityError,
) -> Result<(), Term> {
    context
        .record_activity_failed(Utc::now(), activity_id, error, 1)
        .map_err(|error| context_error_term(&error))
}

fn record_cancelled(context: &NifContext, activity_id: ActivityId) -> Result<(), Term> {
    context
        .record_activity_cancelled(Utc::now(), activity_id)
        .map_err(|error| context_error_term(&error))
}

fn decode_recorded(resolution: Resolution, label: &str) -> Result<ActivityRunResult, Term> {
    match resolution {
        Resolution::ActivityCompleted(payload) => String::from_utf8(payload.bytes().to_vec())
            .map(ActivityRunResult::Completed)
            .map_err(|_| {
                error_result_term(&format!("{label}: recorded activity result is not UTF-8"))
                    .unwrap_or(Term::NIL)
            }),
        Resolution::ActivityFailedTerminal(error) => Ok(ActivityRunResult::Failed(error.message)),
        other => Err(error_result_term(&format!(
            "{label}: recorded non-activity resolution {other:?}"
        ))
        .unwrap_or(Term::NIL)),
    }
}

fn resolve_spec(
    context: &mut NifContext,
    spec: &ActivitySpec,
    label: &str,
) -> Result<(ActivityId, ResolveOutcome, Payload), Term> {
    let input_payload = json_payload(&spec.input, label)?;
    let ordinal = context.next_activity_ordinal();
    let activity_id = ActivityId::from_sequence_position(ordinal);
    let outcome = context
        .resolve_command(Command::RunActivity {
            key: CorrelationKey::Activity(ordinal),
            activity_type: spec.name.clone(),
            input: input_payload.clone(),
        })
        .map_err(|error| context_error_term(&error))?;
    Ok((activity_id, outcome, input_payload))
}

fn run_live_activity(
    context: &NifContext,
    dispatcher: &dyn ActivityDispatcher,
    spec: &ActivitySpec,
    activity_id: ActivityId,
    input_payload: Payload,
) -> Result<ActivityRunResult, Term> {
    record_started(
        context,
        activity_id.clone(),
        spec.name.clone(),
        input_payload,
    )?;
    match dispatcher.dispatch_from_process(
        &spec.name,
        &spec.input,
        &spec.config,
        Some(context.pid()),
    ) {
        Ok(result) => {
            record_completed(context, activity_id, result_payload(&result))?;
            Ok(ActivityRunResult::Completed(result))
        }
        Err(reason) => {
            record_failed(context, activity_id, activity_error(reason.clone()))?;
            Ok(ActivityRunResult::Failed(reason))
        }
    }
}

fn encoded_results(results: &[String]) -> Result<Term, Term> {
    let payload = serde_json::to_vec(results).map_err(|error| {
        error_result_term(&format!("collect: failed to encode result list: {error}"))
            .unwrap_or(Term::NIL)
    })?;
    Ok(ok_result_term(&payload).unwrap_or(Term::NIL))
}

fn missing_dispatcher_term() -> Term {
    error_result_term(
        "no activity dispatcher configured — set one via EngineBuilder::activity_dispatcher",
    )
    .unwrap_or(Term::NIL)
}

fn collect_all_with_context(
    mut context: NifContext,
    dispatcher: Option<&dyn ActivityDispatcher>,
    specs: &[ActivitySpec],
    label: &str,
) -> Result<Term, Term> {
    std::hint::black_box(std::any::type_name::<crate::concurrency::AllRecordingContext>());
    let mut results = Vec::with_capacity(specs.len());
    for spec in specs {
        let (activity_id, outcome, input_payload) = resolve_spec(&mut context, spec, label)?;
        let run_result = match outcome {
            ResolveOutcome::Recorded(resolution) => decode_recorded(resolution, label)?,
            ResolveOutcome::ResumeLive => {
                let Some(dispatcher) = dispatcher else {
                    return Ok(missing_dispatcher_term());
                };
                run_live_activity(&context, dispatcher, spec, activity_id, input_payload)?
            }
        };
        match run_result {
            ActivityRunResult::Completed(result) => results.push(result),
            ActivityRunResult::Failed(reason) => {
                return Ok(error_result_term(&reason).unwrap_or(Term::NIL));
            }
        }
    }
    encoded_results(&results)
}

fn collect_race_with_context(
    mut context: NifContext,
    dispatcher: Option<&dyn ActivityDispatcher>,
    specs: &[ActivitySpec],
) -> Result<Term, Term> {
    std::hint::black_box(std::any::type_name::<
        crate::concurrency::RaceRecordingContext,
    >());
    if specs.is_empty() {
        return Ok(
            error_result_term("collect_race: expected at least one activity").unwrap_or(Term::NIL),
        );
    }
    let Some(dispatcher) = dispatcher else {
        return Ok(missing_dispatcher_term());
    };
    let mut winner = None;
    for spec in specs {
        let (activity_id, outcome, input_payload) =
            resolve_spec(&mut context, spec, "collect_race")?;
        match outcome {
            ResolveOutcome::Recorded(resolution) => {
                let recorded = decode_recorded(resolution, "collect_race")?;
                return match recorded {
                    ActivityRunResult::Completed(result) => {
                        Ok(ok_result_term(result.as_bytes()).unwrap_or(Term::NIL))
                    }
                    ActivityRunResult::Failed(reason) => {
                        Ok(error_result_term(&reason).unwrap_or(Term::NIL))
                    }
                };
            }
            ResolveOutcome::ResumeLive => {
                record_started(
                    &context,
                    activity_id.clone(),
                    spec.name.clone(),
                    input_payload,
                )?;
                if winner.is_none() {
                    let result = match dispatcher.dispatch_from_process(
                        &spec.name,
                        &spec.input,
                        &spec.config,
                        Some(context.pid()),
                    ) {
                        Ok(result) => {
                            record_completed(
                                &context,
                                activity_id.clone(),
                                result_payload(&result),
                            )?;
                            ActivityRunResult::Completed(result)
                        }
                        Err(reason) => {
                            record_failed(
                                &context,
                                activity_id.clone(),
                                activity_error(reason.clone()),
                            )?;
                            ActivityRunResult::Failed(reason)
                        }
                    };
                    winner = Some(result);
                } else {
                    record_cancelled(&context, activity_id)?;
                }
            }
        }
    }
    match winner {
        Some(ActivityRunResult::Completed(result)) => {
            Ok(ok_result_term(result.as_bytes()).unwrap_or(Term::NIL))
        }
        Some(ActivityRunResult::Failed(reason)) => {
            Ok(error_result_term(&reason).unwrap_or(Term::NIL))
        }
        None => Ok(error_result_term("collect_race: no winner recorded").unwrap_or(Term::NIL)),
    }
}

fn context_from_process(ctx: &ProcessContext, label: &str) -> Result<NifContext, Term> {
    let Some(pid) = ctx.pid() else {
        return Err(
            error_result_term(&format!("{label}: missing calling process pid"))
                .unwrap_or(Term::NIL),
        );
    };
    let runtime = runtime_context().map_err(|error| context_error_term(&error))?;
    NifContext::new(pid, runtime.registry.as_ref(), runtime.tokio_handle)
        .map_err(|error| context_error_term(&error))
}

fn decode_concurrency_args(args: &[Term], label: &str) -> Result<Vec<ActivitySpec>, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    if args.len() != 2 {
        return Err(error_result_term(&format!(
            "{label}: expected 2 arguments, got {}",
            args.len()
        ))
        .unwrap_or(Term::NIL));
    }
    decode_string_arg(args[0]).map_err(|error| {
        error_result_term(&format!("{label}: collection id: {error}")).unwrap_or(Term::NIL)
    })?;
    decode_spec_list(args[1], label)
}

/// NIF backing `aion_flow_ffi:collect_all/2`.
pub(super) fn collect_all_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    let specs = match decode_concurrency_args(args, "collect_all") {
        Ok(specs) => specs,
        Err(term) => return Ok(term),
    };
    let context = match context_from_process(ctx, "collect_all") {
        Ok(context) => context,
        Err(term) => return Ok(term),
    };
    let dispatcher = activity_dispatcher();
    collect_all_with_context(context, dispatcher.as_deref(), &specs, "collect_all")
}

/// NIF backing `aion_flow_ffi:collect_race/2`.
pub(super) fn collect_race_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    let specs = match decode_concurrency_args(args, "collect_race") {
        Ok(specs) => specs,
        Err(term) => return Ok(term),
    };
    let context = match context_from_process(ctx, "collect_race") {
        Ok(context) => context,
        Err(term) => return Ok(term),
    };
    let dispatcher = activity_dispatcher();
    collect_race_with_context(context, dispatcher.as_deref(), &specs)
}

/// NIF backing `aion_flow_ffi:collect_map/2`.
pub(super) fn collect_map_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    let specs = match decode_concurrency_args(args, "collect_map") {
        Ok(specs) => specs,
        Err(term) => return Ok(term),
    };
    let context = match context_from_process(ctx, "collect_map") {
        Ok(context) => context,
        Err(term) => return Ok(term),
    };
    let dispatcher = activity_dispatcher();
    std::hint::black_box(std::any::type_name::<crate::concurrency::AllRecordingContext>());
    collect_all_with_context(context, dispatcher.as_deref(), &specs, "collect_map")
}
