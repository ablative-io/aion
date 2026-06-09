//! Engine-owned NIF implementations for the `aion_flow_ffi` namespace.
//!
//! These NIFs back the `@external(erlang, "aion_flow_ffi", ...)` declarations
//! in the Gleam `aion_flow` SDK. `run_activity` is registered as a dirty NIF
//! because activity dispatch may block on network I/O.

use std::cell::RefCell;

use beamr::atom::Atom;
use beamr::native::ProcessContext;
use beamr::term::Term;
use beamr::term::binary::{self, Binary};
use beamr::term::boxed;

use super::nif::{Mfa, NifEntry};
use super::nif_determinism::{now_impl, random_impl, random_int_impl};
use super::nif_signal;
use super::nif_timer;

const FFI_MODULE: &str = "aion_flow_ffi";
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


/// NIF backing `aion_flow_ffi:run_activity/3`.
///
/// Heap from the previous NIF invocation is drained first (matching
/// `NifContext`'s one-call retention window), then fresh allocations for
/// the return value are parked for the scheduler to copy.
fn run_activity(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    super::nif_activity::run_activity_impl(args, ctx)
}

fn not_yet_implemented(args: &[Term], ctx: &mut ProcessContext) -> Result<Term, Term> {
    let _ = ctx.pid();
    if args.len() > 255 {
        return Err(Term::NIL);
    }

    Ok(error_result_term(NOT_YET_IMPLEMENTED).unwrap_or(Term::NIL))
}

fn dirty_entry(function: &str, arity: u8) -> NifEntry {
    NifEntry::dirty(Mfa::new(FFI_MODULE, function, arity), not_yet_implemented)
}

/// Collect engine-owned NIF entries for `aion_flow_ffi`.
pub(super) fn engine_nif_entries() -> Vec<NifEntry> {
    vec![
        NifEntry::dirty(Mfa::new(FFI_MODULE, "run_activity", 3), run_activity),
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
        NifEntry::dirty(
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
        dirty_entry("register_query", 3),
        dirty_entry("reply_query", 2),
        dirty_entry("dispatch_query", 2),
        dirty_entry("spawn_child", 3),
        dirty_entry("await_child", 1),
        dirty_entry("collect_all", 2),
        dirty_entry("collect_race", 2),
        dirty_entry("collect_map", 2),
    ]
}

#[cfg(test)]
mod tests {
    use beamr::native::ProcessContext;
    use beamr::term::Term;
    use beamr::term::binary::Binary;
    use beamr::term::boxed::Tuple;

    use super::{
        NOT_YET_IMPLEMENTED, alloc_binary_term, clear_parked_heap, engine_nif_entries,
        not_yet_implemented, run_activity,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn binary_arg(value: &str) -> Term {
        alloc_binary_term(value.as_bytes()).unwrap_or(Term::NIL)
    }

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
    fn returns_result_tuple_for_valid_call() -> TestResult {
        use std::sync::Arc;

        use crate::activity::bridge::{ActivityDispatcher, install_activity_dispatcher};

        struct TestDispatcher;
        impl ActivityDispatcher for TestDispatcher {
            fn dispatch(&self, _name: &str, input: &str, _config: &str) -> Result<String, String> {
                Ok(format!("dispatched:{input}"))
            }
        }
        install_activity_dispatcher(Arc::new(TestDispatcher));

        clear_parked_heap();
        let name = binary_arg("greet");
        let input = binary_arg("{\"name\":\"Alice\"}");
        let config = binary_arg("{}");
        let mut ctx = ProcessContext::new();

        let result = run_activity(&[name, input, config], &mut ctx);

        match result {
            Ok(term) => {
                let (tag, _value) = decode_result_tuple(term)?;
                assert!(
                    tag == "ok" || tag == "error",
                    "result should be a tagged tuple"
                );
            }
            Err(_) => return Err("NIF should return Ok at the beamr level".into()),
        }
        Ok(())
    }

    #[test]
    fn returns_error_on_wrong_arity() -> TestResult {
        clear_parked_heap();
        let mut ctx = ProcessContext::new();

        let result = run_activity(&[], &mut ctx);

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
    fn registers_all_engine_nifs_as_unique_entries_with_pure_determinism_nifs() {
        let entries = engine_nif_entries();
        let unique = entries
            .iter()
            .map(|entry| entry.mfa.display())
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(entries.len(), 18);
        assert_eq!(unique.len(), entries.len());
        for deterministic in ["now", "random", "random_int"] {
            let found = entries
                .iter()
                .any(|entry| entry.mfa.function == deterministic && !entry.is_dirty);
            assert!(found, "{deterministic} should be a registered normal NIF");
        }
        assert!(
            entries
                .iter()
                .filter(|entry| !matches!(
                    entry.mfa.function.as_str(),
                    "now" | "random" | "random_int"
                ))
                .all(|entry| entry.is_dirty)
        );
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
