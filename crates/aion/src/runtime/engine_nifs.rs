//! Engine-owned NIF implementations for the `aion_flow_ffi` namespace.
//!
//! These NIFs back the `@external(erlang, "aion_flow_ffi", ...)` declarations
//! in the Gleam `aion_flow` SDK. Activity dispatch is split into a normal
//! dispatch NIF plus a selective-receive await NIF.

use std::cell::RefCell;

use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary::{self, Binary};
use beamr::term::boxed;

use super::nif::{Mfa, NifEntry};
use super::nif_child;
use super::nif_determinism::{now_impl, random_impl, random_int_impl};
use super::nif_signal;
use super::nif_timer;

const FFI_MODULE: &str = "aion_flow_ffi";
#[cfg(test)]
const NOT_YET_IMPLEMENTED: &str = "not_yet_implemented";

thread_local! {
    static NIF_HEAP: RefCell<Vec<Box<[u64]>>> = const { RefCell::new(Vec::new()) };
}

fn park_heap(heap: Box<[u64]>) {
    NIF_HEAP.with_borrow_mut(|parked| parked.push(heap));
}

#[cfg(test)]
fn clear_parked_heap() {
    NIF_HEAP.with_borrow_mut(Vec::clear);
}

pub(super) fn alloc_binary_term(bytes: &[u8]) -> Option<Term> {
    let word_count = 2 + binary::packed_word_count(bytes.len());
    let mut heap = vec![0_u64; word_count].into_boxed_slice();
    let term = binary::write_binary(&mut heap, bytes)?;
    park_heap(heap);
    Some(term)
}

pub(super) fn alloc_tuple_term(elements: &[Term]) -> Option<Term> {
    let word_count = 1 + elements.len();
    let mut heap = vec![0_u64; word_count].into_boxed_slice();
    let term = boxed::write_tuple(&mut heap, elements)?;
    park_heap(heap);
    Some(term)
}

pub(super) fn ok_result_term(value: &str) -> Option<Term> {
    let value_term = alloc_binary_term(value.as_bytes())?;
    alloc_tuple_term(&[Term::atom(Atom::OK), value_term])
}

pub(super) fn error_result_term(message: &str) -> Option<Term> {
    let value_term = alloc_binary_term(message.as_bytes())?;
    alloc_tuple_term(&[Term::atom(Atom::ERROR), value_term])
}

pub(super) fn decode_string_arg(term: Term) -> Result<String, String> {
    let bin = Binary::new(term).ok_or_else(|| "argument is not a binary".to_owned())?;
    String::from_utf8(bin.as_bytes().to_vec()).map_err(|_| "argument is not valid UTF-8".to_owned())
}

fn dispatch_activity(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    super::nif_activity_dispatch::dispatch_activity_impl(args, ctx)
}

fn await_activity_result(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    super::nif_activity_dispatch::await_activity_result_impl(args, ctx)
}

fn collect_all(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    super::nif_concurrency::collect_all_impl(args, ctx)
}

fn collect_race(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    super::nif_concurrency::collect_race_impl(args, ctx)
}

fn collect_map(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    super::nif_concurrency::collect_map_impl(args, ctx)
}

#[cfg(test)]
fn not_yet_implemented(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    let _ = ctx.pid();
    if args.len() > 255 {
        return Err(Term::NIL);
    }

    Ok(error_result_term(NOT_YET_IMPLEMENTED).unwrap_or(Term::NIL))
}

/// Collect engine-owned NIF entries for `aion_flow_ffi`.
pub(super) fn engine_nif_entries() -> Vec<NifEntry> {
    vec![
        NifEntry::new(
            Mfa::new(FFI_MODULE, "dispatch_activity", 3),
            dispatch_activity,
        ),
        NifEntry::new(
            Mfa::new(FFI_MODULE, "await_activity_result", 1),
            await_activity_result,
        ),
        NifEntry::new(Mfa::new(FFI_MODULE, "now", 0), now_impl),
        NifEntry::new(Mfa::new(FFI_MODULE, "random", 0), random_impl),
        NifEntry::new(Mfa::new(FFI_MODULE, "random_int", 2), random_int_impl),
        NifEntry::dirty(Mfa::new(FFI_MODULE, "sleep", 1), nif_timer::sleep_impl),
        NifEntry::dirty(
            Mfa::new(FFI_MODULE, "start_timer", 2),
            nif_timer::start_timer_impl,
        ),
        NifEntry::dirty(
            Mfa::new(FFI_MODULE, "cancel_timer", 1),
            nif_timer::cancel_timer_impl,
        ),
        NifEntry::new(
            Mfa::new(FFI_MODULE, "with_timeout", 2),
            nif_timer::with_timeout_impl,
        ),
        NifEntry::dirty(
            Mfa::new(FFI_MODULE, "receive_signal", 2),
            nif_signal::receive_signal,
        ),
        NifEntry::dirty(
            Mfa::new(FFI_MODULE, "send_signal", 3),
            nif_signal::send_signal,
        ),
        NifEntry::new(
            Mfa::new(FFI_MODULE, "register_query", 3),
            super::nif_query::register_query,
        ),
        NifEntry::dirty(
            Mfa::new(FFI_MODULE, "reply_query", 2),
            super::nif_query::reply_query,
        ),
        NifEntry::dirty(
            Mfa::new(FFI_MODULE, "dispatch_query", 2),
            super::nif_query::dispatch_query,
        ),
        NifEntry::dirty(
            Mfa::new(FFI_MODULE, "spawn_child", 3),
            nif_child::spawn_child_impl,
        ),
        NifEntry::dirty(
            Mfa::new(FFI_MODULE, "await_child", 1),
            nif_child::await_child_impl,
        ),
        NifEntry::dirty(Mfa::new(FFI_MODULE, "collect_all", 2), collect_all),
        NifEntry::dirty(Mfa::new(FFI_MODULE, "collect_race", 2), collect_race),
        NifEntry::dirty(Mfa::new(FFI_MODULE, "collect_map", 2), collect_map),
    ]
}

#[cfg(test)]
mod tests {
    use beamr::native::ProcessContext;
    use beamr::term::Term;
    use beamr::term::binary::Binary;
    use beamr::term::boxed::Tuple;

    use super::{
        NOT_YET_IMPLEMENTED, clear_parked_heap, dispatch_activity, engine_nif_entries,
        not_yet_implemented,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn decode_result_tuple(term: Term) -> Result<(String, String), Box<dyn std::error::Error>> {
        let tuple = Tuple::new(term).ok_or("result should be a tuple")?;
        if tuple.arity() != 2 {
            return Err(format!("expected arity 2, got {}", tuple.arity()).into());
        }
        let tag = tuple.get(0).ok_or("missing tag element")?;
        let value = tuple.get(1).ok_or("missing value element")?;
        let tag_name = if tag == Term::atom(beamr::atom::Atom::OK) {
            "ok"
        } else {
            "error"
        };
        let bin = Binary::new(value).ok_or("value should be a binary")?;
        let text = String::from_utf8(bin.as_bytes().to_vec())
            .map_err(|_| "value should be valid UTF-8")?;
        Ok((tag_name.to_owned(), text))
    }

    #[test]
    fn returns_error_on_wrong_arity() -> TestResult {
        clear_parked_heap();
        let mut ctx = ProcessContext::new();

        let result = dispatch_activity(&[], &mut ctx);

        match result {
            Ok(term) => {
                let (tag, message) = decode_result_tuple(term)?;
                assert_eq!(tag, "error");
                assert!(
                    message.contains("expected 3 arguments"),
                    "unexpected: {message}"
                );
            }
            Err(_) => return Err("NIF should return Ok at the beamr level".into()),
        }
        Ok(())
    }

    #[test]
    fn registers_all_engine_nifs_as_unique_entries_with_correct_scheduling() -> TestResult {
        let entries = engine_nif_entries();
        let unique = entries
            .iter()
            .map(|entry| entry.mfa.display())
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(entries.len(), 19);
        assert_eq!(unique.len(), entries.len());
        for normal_nif in [
            "dispatch_activity",
            "await_activity_result",
            "now",
            "random",
            "random_int",
            "with_timeout",
            "register_query",
        ] {
            let found = entries
                .iter()
                .any(|entry| entry.mfa.function == normal_nif && !entry.is_dirty);
            assert!(found, "{normal_nif} should be a registered normal NIF");
        }
        assert!(
            entries
                .iter()
                .filter(|entry| !matches!(
                    entry.mfa.function.as_str(),
                    "dispatch_activity"
                        | "await_activity_result"
                        | "now"
                        | "random"
                        | "random_int"
                        | "with_timeout"
                        | "register_query"
                ))
                .all(|entry| entry.is_dirty)
        );
        for name in [
            "collect_all",
            "collect_race",
            "collect_map",
            "register_query",
            "reply_query",
            "dispatch_query",
            "spawn_child",
            "await_child",
        ] {
            let entry = entries
                .iter()
                .find(|entry| entry.mfa.function == name)
                .ok_or_else(|| format!("missing {name}"))?;
            assert!(
                !std::ptr::fn_addr_eq(
                    entry.function,
                    not_yet_implemented as beamr::native::NativeFn
                ),
                "{name} should not use the stub"
            );
        }
        Ok(())
    }

    #[test]
    fn unimplemented_stub_returns_standard_error_tuple() -> TestResult {
        clear_parked_heap();
        let mut ctx = ProcessContext::new();

        let result = not_yet_implemented(&[], &mut ctx);

        match result {
            Ok(term) => {
                let (tag, message) = decode_result_tuple(term)?;
                assert_eq!(tag, "error");
                assert_eq!(message, NOT_YET_IMPLEMENTED);
            }
            Err(_) => return Err("stub should return Ok at the beamr level".into()),
        }
        Ok(())
    }
}
