//! Two-phase suspending `collect_*` NIFs over parallel activity dispatch.
//!
//! `collect_all`/`collect_race`/`collect_map` fan out N activities and park
//! the workflow process instead of blocking a dirty thread. This module is
//! the BEAM-facing shell — argument decoding, result-term encoding, the
//! servicing guard, and wake-marker consumption; one full resolution pass
//! per invocation (pin, batch record/dispatch, per-ordinal sweep,
//! settlement) lives in [`super::nif_collect`].

use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary_ref::BinaryRef;
use beamr::term::boxed::Cons;

use crate::runtime::nif_activity::runtime_context;
use crate::runtime::nif_collect::{ActivitySpec, CollectDeps, CollectStep, collect_step};
use crate::runtime::nif_state::{CollectKind, engine_nif_state};

/// Build `{Tag, <<bytes>>}` on the calling process heap.
///
/// Result terms are allocated through the [`ProcessContext`] allocators:
/// attached (normal-scheduler) calls get GC-traced process-heap terms, and
/// detached (dirty) calls get owned blocks the dirty-result bridge copies
/// onto the process heap. Nothing is parked in thread-locals — beamr's
/// moving GC never traces out-of-heap pointers, so a parked heap either
/// leaks for the scheduler thread's lifetime or dangles once cleared while
/// workflow code still references the term (N-6).
///
/// Allocation may collect on attached calls: never touch an argument `Term`
/// (including cons cells of the spec list) after a result allocation.
fn tagged_result_term(ctx: &mut ProcessContext, tag: Atom, bytes: &[u8]) -> Option<Term> {
    let value = ctx.alloc_binary(bytes).ok()?;
    ctx.alloc_tuple(&[Term::atom(tag), value]).ok()
}

fn ok_result_term(ctx: &mut ProcessContext, bytes: &[u8]) -> Option<Term> {
    tagged_result_term(ctx, Atom::OK, bytes)
}

fn error_result_term(ctx: &mut ProcessContext, message: &str) -> Option<Term> {
    tagged_result_term(ctx, Atom::ERROR, message.as_bytes())
}

fn decode_string_arg(term: Term) -> Result<String, String> {
    let bin = BinaryRef::new(term).ok_or_else(|| "argument is not a binary".to_owned())?;
    String::from_utf8(bin.as_bytes().to_vec()).map_err(|_| "argument is not valid UTF-8".to_owned())
}

fn decode_spec_list(
    ctx: &mut ProcessContext,
    term: Term,
    label: &str,
) -> Result<Vec<ActivitySpec>, Term> {
    let mut specs = Vec::new();
    let mut tail = term;
    while !tail.is_nil() {
        // Every error-term allocation below may collect and move the list
        // being walked, so each failure path allocates and returns without
        // touching `cons`/`tail` again.
        let Some(cons) = Cons::new(tail) else {
            return Err(error_result_term(
                ctx,
                &format!("{label}: activities argument is not a proper list"),
            )
            .unwrap_or(Term::NIL));
        };
        let head = cons.head();
        let next_tail = cons.tail();
        let encoded = decode_string_arg(head).map_err(|error| {
            error_result_term(ctx, &format!("{label}: activity spec: {error}")).unwrap_or(Term::NIL)
        })?;
        let spec = serde_json::from_str(&encoded).map_err(|error| {
            error_result_term(
                ctx,
                &format!("{label}: invalid activity spec JSON: {error}"),
            )
            .unwrap_or(Term::NIL)
        })?;
        specs.push(spec);
        tail = next_tail;
    }
    Ok(specs)
}

fn decode_concurrency_args(
    ctx: &mut ProcessContext,
    args: &[Term],
    label: &str,
) -> Result<Vec<ActivitySpec>, Term> {
    if args.len() != 2 {
        return Err(error_result_term(
            ctx,
            &format!("{label}: expected 2 arguments, got {}", args.len()),
        )
        .unwrap_or(Term::NIL));
    }
    decode_string_arg(args[0]).map_err(|error| {
        error_result_term(ctx, &format!("{label}: collection id: {error}")).unwrap_or(Term::NIL)
    })?;
    decode_spec_list(ctx, args[1], label)
}

fn encoded_results(ctx: &mut ProcessContext, results: &[String]) -> Term {
    match serde_json::to_vec(results) {
        Ok(payload) => ok_result_term(ctx, &payload).unwrap_or(Term::NIL),
        Err(error) => error_result_term(
            ctx,
            &format!("collect: failed to encode result list: {error}"),
        )
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
    let specs = match decode_concurrency_args(ctx, args, label) {
        Ok(specs) => specs,
        Err(term) => return Ok(term),
    };
    let Some(pid) = ctx.pid() else {
        return Ok(
            error_result_term(ctx, &format!("{label}: missing calling process pid"))
                .unwrap_or(Term::NIL),
        );
    };
    let state = match engine_nif_state(ctx) {
        Ok(state) => state,
        Err(error) => return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL)),
    };
    // Every collect_* NIF records activity events; a query handler must stay
    // read-only. The refusal precedes the marker consumption so a refused
    // handler call never eats a wake.
    if let Err(error) = super::nif_query_pump::ensure_not_servicing_query(&state, pid, label) {
        return Ok(error_result_term(ctx, &error).unwrap_or(Term::NIL));
    }
    let runtime = match runtime_context(&state) {
        Ok(runtime) => runtime,
        Err(error) => return Ok(error_result_term(ctx, &error.to_string()).unwrap_or(Term::NIL)),
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
            Ok(error_result_term(ctx, &sentinel).unwrap_or(Term::NIL))
        }
        Ok(CollectStep::AllCompleted(results)) => Ok(encoded_results(ctx, &results)),
        Ok(CollectStep::RaceWon(Ok(payload))) => {
            Ok(ok_result_term(ctx, payload.as_bytes()).unwrap_or(Term::NIL))
        }
        Ok(
            CollectStep::RaceWon(Err(message))
            | CollectStep::FailFast(message)
            | CollectStep::ScopeExpired(message),
        ) => Ok(error_result_term(ctx, &message).unwrap_or(Term::NIL)),
        Ok(CollectStep::Suspend) => {
            // Park the process; the next mailbox wake re-invokes this native
            // from the top with the ordinal base pinned. The NIL return is
            // never observed by workflow code.
            ctx.request_suspend(None);
            Ok(Term::NIL)
        }
        Err(message) => {
            Ok(error_result_term(ctx, &format!("{label}:{message}")).unwrap_or(Term::NIL))
        }
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
