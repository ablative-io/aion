//! Two-phase suspending `collect_*` NIFs over parallel activity dispatch.
//!
//! `collect_all`/`collect_race`/`collect_map` fan out N activities and park
//! the workflow process instead of blocking a dirty thread. This module is
//! the BEAM-facing shell — argument decoding, result-term encoding, the
//! servicing guard, and wake-marker consumption; one full resolution pass
//! per invocation (pin, batch record/dispatch, per-ordinal sweep,
//! settlement) lives in [`super::nif_collect`].

use std::cell::RefCell;

use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary;
use beamr::term::binary_ref::BinaryRef;
use beamr::term::boxed::{self, Cons};

use crate::runtime::nif_activity::runtime_context;
use crate::runtime::nif_collect::{ActivitySpec, CollectDeps, CollectStep, collect_step};
use crate::runtime::nif_state::{CollectKind, engine_nif_state};

thread_local! {
    static CONCURRENCY_NIF_HEAP: RefCell<Vec<Box<[u64]>>> = const { RefCell::new(Vec::new()) };
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

fn decode_string_arg(term: Term) -> Result<String, String> {
    let bin = BinaryRef::new(term).ok_or_else(|| "argument is not a binary".to_owned())?;
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

fn decode_concurrency_args(args: &[Term], label: &str) -> Result<Vec<ActivitySpec>, Term> {
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

fn encoded_results(results: &[String]) -> Term {
    match serde_json::to_vec(results) {
        Ok(payload) => ok_result_term(&payload).unwrap_or(Term::NIL),
        Err(error) => error_result_term(&format!("collect: failed to encode result list: {error}"))
            .unwrap_or(Term::NIL),
    }
}

fn run_collect(
    args: &[Term],
    ctx: &mut ProcessContext,
    kind: CollectKind,
    label: &str,
) -> Result<Term, Term> {
    if args.len() > 255 {
        return Err(Term::NIL);
    }
    let specs = match decode_concurrency_args(args, label) {
        Ok(specs) => specs,
        Err(term) => return Ok(term),
    };
    let Some(pid) = ctx.pid() else {
        return Ok(
            error_result_term(&format!("{label}: missing calling process pid"))
                .unwrap_or(Term::NIL),
        );
    };
    let state = match engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(&error).unwrap_or(Term::NIL)),
    };
    // Every collect_* NIF records activity events; a query handler must stay
    // read-only. The refusal precedes the marker consumption so a refused
    // handler call never eats a wake.
    if let Err(error) = super::nif_query_pump::ensure_not_servicing_query(&state, pid, label) {
        return Ok(error_result_term(&error).unwrap_or(Term::NIL));
    }
    let runtime = match runtime_context(&state) {
        Ok(runtime) => runtime,
        Err(error) => return Ok(error_result_term(&error.to_string()).unwrap_or(Term::NIL)),
    };
    // One wake marker is consumed per invocation; leaving it queued would
    // insta-rewake the suspend below into a busy spin.
    super::nif_wake::consume_wake_marker(ctx, &runtime.runtime);
    let deps = CollectDeps {
        registry: runtime.registry,
        runtime: runtime.runtime,
        tokio_handle: runtime.tokio_handle,
        dispatcher: state.activity_dispatcher(),
    };
    match collect_step(&state, &deps, pid, kind, &specs, label) {
        Ok(CollectStep::QuerySentinel(sentinel)) => {
            Ok(error_result_term(&sentinel).unwrap_or(Term::NIL))
        }
        Ok(CollectStep::AllCompleted(results)) => Ok(encoded_results(&results)),
        Ok(CollectStep::RaceWon(Ok(payload))) => {
            Ok(ok_result_term(payload.as_bytes()).unwrap_or(Term::NIL))
        }
        Ok(
            CollectStep::RaceWon(Err(message))
            | CollectStep::FailFast(message)
            | CollectStep::ScopeExpired(message),
        ) => Ok(error_result_term(&message).unwrap_or(Term::NIL)),
        Ok(CollectStep::Suspend) => {
            // Park the process; the next mailbox wake re-invokes this native
            // from the top with the ordinal base pinned. The NIL return is
            // never observed by workflow code.
            ctx.request_suspend(None);
            Ok(Term::NIL)
        }
        Err(message) => Ok(error_result_term(&format!("{label}:{message}")).unwrap_or(Term::NIL)),
    }
}

/// NIF backing `aion_flow_ffi:collect_all/2`.
pub(super) fn collect_all_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    run_collect(args, ctx, CollectKind::All, "collect_all")
}

/// NIF backing `aion_flow_ffi:collect_race/2`.
pub(super) fn collect_race_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    run_collect(args, ctx, CollectKind::Race, "collect_race")
}

/// NIF backing `aion_flow_ffi:collect_map/2`.
pub(super) fn collect_map_impl(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    run_collect(args, ctx, CollectKind::All, "collect_map")
}
