//! Static determinism analysis over a workflow entry-module's Gleam source.
//!
//! The determinism boundary (invariant 2) requires workflow code to be a pure
//! function of its recorded history: time comes from `workflow.now`, entropy
//! from `workflow.random` / `workflow.random_int`, both recorded and replayed.
//! A direct wall-clock read or entropy draw — `gleam/erlang.system_time`,
//! `gleam/float.random`, `erlang:unique_integer`, … — is invisible to the
//! recorder, so a replay re-runs it and silently diverges. This analysis is the
//! static gate that catches that class before it ships (P7, C28).
//!
//! It reuses the [`super::scan`] tokeniser and the same control-flow primitive
//! the extractor uses (function-body mapping plus reachability over local helper
//! calls), so the linter and the graph projection agree on what "reachable from
//! workflow code" means. Reachability follows a helper both when it is applied
//! directly (`helper(..)`) and when it is passed as a bare function value into a
//! higher-order call (`list.map(items, helper)`), so a forbidden call hidden
//! behind a passed helper is not silently missed. It is deliberately not a Gleam
//! type-checker: the recognised non-deterministic vocabulary is a fixed, known
//! set of qualified-call shapes, matched soundly because string literals and
//! comments are excluded by the tokeniser. A call outside the known set is never
//! flagged (no false positive on the author's own helpers); a known call
//! reachable from the entry function is always flagged (no silent miss). The one
//! reachability edge not followed is a helper aliased through a `let` binding and
//! then passed (`let f = helper  list.map(items, f)`): name-level token analysis
//! cannot resolve the alias without a type-checker, and the deterministic SDK
//! surface gives authors no reason to write it.
//!
//! The vocabulary covers the wall-clock and entropy sources reachable from the
//! BEAM's Gleam surface — Erlang's `os`/`erlang` time and uniqueness builtins,
//! `gleam/erlang` time wrappers, and the `gleam/int` / `gleam/float` random
//! draws. The deterministic SDK surface (`workflow.now`, `workflow.random`,
//! `workflow.random_int`) is explicitly NOT in the set: those are the recorded,
//! replay-safe substitutes the boundary mandates.

use std::collections::{BTreeMap, BTreeSet};

use super::reader::{find_open_brace, match_brace};
use super::scan::{Token, tokenise};

/// One recognised non-deterministic call shape: the module qualifier left of the
/// dot, the member right of it, and a human description of why it is forbidden.
struct ForbiddenCall {
    /// The qualifier as written at a recognised call site (a `gleam/erlang`
    /// import is referenced as `erlang.<member>`; `gleam/float` as
    /// `float.<member>`; an `@external` Erlang call as `os` / `erlang` /
    /// `rand` / `crypto`).
    qualifier: &'static str,
    /// The member right of the dot.
    member: &'static str,
    /// Whether this is a wall-clock read or an entropy draw, for the diagnostic.
    kind: ViolationKind,
}

/// Whether a flagged call reads the wall clock or draws entropy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ViolationKind {
    /// A wall-clock read: the recorded `workflow.now()` is the only legal time
    /// source inside workflow code.
    WallClock,
    /// An entropy draw: `workflow.random()` / `workflow.random_int(..)` are the
    /// only legal entropy sources inside workflow code.
    Entropy,
}

impl ViolationKind {
    /// The deterministic SDK substitute an author should reach for instead.
    #[must_use]
    pub fn remedy(self) -> &'static str {
        match self {
            ViolationKind::WallClock => {
                "use the recorded `workflow.now()` instead of reading the wall clock"
            }
            ViolationKind::Entropy => {
                "use the seeded `workflow.random()` / `workflow.random_int(..)` \
                 instead of drawing entropy"
            }
        }
    }
}

/// The fixed vocabulary of non-deterministic calls the gate flags. Each entry is
/// a wall-clock or entropy source reachable from Gleam workflow code on the BEAM;
/// the deterministic `workflow.*` substitutes are deliberately absent.
const FORBIDDEN_CALLS: &[ForbiddenCall] = &[
    // Erlang time builtins, reached via an `@external(erlang, "erlang", ..)` or
    // `@external(erlang, "os", ..)` declaration whose Gleam name keeps the
    // module qualifier, or via `gleam/erlang/os` / `gleam/erlang` wrappers.
    ForbiddenCall {
        qualifier: "erlang",
        member: "system_time",
        kind: ViolationKind::WallClock,
    },
    ForbiddenCall {
        qualifier: "erlang",
        member: "monotonic_time",
        kind: ViolationKind::WallClock,
    },
    ForbiddenCall {
        qualifier: "erlang",
        member: "now",
        kind: ViolationKind::WallClock,
    },
    ForbiddenCall {
        qualifier: "erlang",
        member: "timestamp",
        kind: ViolationKind::WallClock,
    },
    ForbiddenCall {
        qualifier: "erlang",
        member: "unique_integer",
        kind: ViolationKind::Entropy,
    },
    ForbiddenCall {
        qualifier: "os",
        member: "system_time",
        kind: ViolationKind::WallClock,
    },
    ForbiddenCall {
        qualifier: "os",
        member: "timestamp",
        kind: ViolationKind::WallClock,
    },
    ForbiddenCall {
        qualifier: "os",
        member: "perf_counter",
        kind: ViolationKind::WallClock,
    },
    // `gleam/erlang/os` re-exports the wall clock under the same member names,
    // referenced as `os.<member>` once imported.
    ForbiddenCall {
        qualifier: "os",
        member: "erlang_timestamp",
        kind: ViolationKind::WallClock,
    },
    // Erlang entropy builtins.
    ForbiddenCall {
        qualifier: "rand",
        member: "uniform",
        kind: ViolationKind::Entropy,
    },
    ForbiddenCall {
        qualifier: "rand",
        member: "uniform_real",
        kind: ViolationKind::Entropy,
    },
    ForbiddenCall {
        qualifier: "rand",
        member: "bytes",
        kind: ViolationKind::Entropy,
    },
    ForbiddenCall {
        qualifier: "crypto",
        member: "strong_rand_bytes",
        kind: ViolationKind::Entropy,
    },
    // `gleam/float` and `gleam/int` random draws (the stdlib's non-deterministic
    // surface), referenced as `float.random` / `int.random`.
    ForbiddenCall {
        qualifier: "float",
        member: "random",
        kind: ViolationKind::Entropy,
    },
    ForbiddenCall {
        qualifier: "int",
        member: "random",
        kind: ViolationKind::Entropy,
    },
];

/// A single flagged non-deterministic call site reachable from workflow code.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Violation {
    /// The function the call was found in (the entry function, or a local helper
    /// reachable from it).
    pub function: String,
    /// The fully-qualified call as written (`erlang.system_time`).
    pub call: String,
    /// Whether the call reads the wall clock or draws entropy.
    pub kind: ViolationKind,
}

/// Errors raised before analysis can run.
#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum DeterminismError {
    /// The named entry function is not defined in the supplied source, so the
    /// reachability walk has no root. Reported loudly rather than passing a
    /// workflow whose entry the analyser never reached.
    #[error(
        "entry function `{function}` is not defined in the workflow source; the determinism \
         analysis requires its body to walk from"
    )]
    EntryFunctionNotFound {
        /// The entry-function name searched for.
        function: String,
    },
}

/// A function body as the half-open token range strictly inside its `{ }`.
#[derive(Clone, Copy)]
struct FnBody {
    start: usize,
    end: usize,
}

/// Analyses `source` for wall-clock and entropy calls reachable from
/// `entry_function`, following local helper calls.
///
/// Returns every flagged call in deterministic order (by function name, then
/// call, then kind), so a clean workflow yields an empty vector and a tainted
/// one yields a stable, reproducible report.
///
/// # Errors
///
/// Returns [`DeterminismError::EntryFunctionNotFound`] when `entry_function` is
/// not defined in `source`.
pub fn analyze_determinism(
    source: &str,
    entry_function: &str,
) -> Result<Vec<Violation>, DeterminismError> {
    let tokens = tokenise(source);
    let functions = map_functions(&tokens);
    if !functions.contains_key(entry_function) {
        return Err(DeterminismError::EntryFunctionNotFound {
            function: entry_function.to_owned(),
        });
    }

    let mut violations: BTreeSet<Violation> = BTreeSet::new();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    walk(
        &tokens,
        &functions,
        entry_function,
        &mut visited,
        &mut violations,
    );
    Ok(violations.into_iter().collect())
}

/// Walks `function`'s body, recording every forbidden call and recursing into
/// every reachable local helper exactly once (guarded by `visited`, so mutual
/// recursion terminates).
fn walk(
    tokens: &[Token],
    functions: &BTreeMap<String, FnBody>,
    function: &str,
    visited: &mut BTreeSet<String>,
    violations: &mut BTreeSet<Violation>,
) {
    if !visited.insert(function.to_owned()) {
        return;
    }
    let Some(body) = functions.get(function).copied() else {
        return;
    };
    // Collect the helper calls to recurse into after scanning this body, so the
    // immutable borrow of `tokens` for forbidden-call matching does not overlap
    // the recursive descent.
    let mut callees: Vec<String> = Vec::new();
    let upper = body.end.min(tokens.len());
    let mut index = body.start;
    let mut depth: usize = 0;
    while index < upper {
        match &tokens[index] {
            Token::OpenParen => depth += 1,
            Token::CloseParen => depth = depth.saturating_sub(1),
            Token::Qualified { left, right } => {
                if let Some(forbidden) = match_forbidden(left, right) {
                    violations.insert(Violation {
                        function: function.to_owned(),
                        call: format!("{left}.{right}"),
                        kind: forbidden.kind,
                    });
                }
            }
            // A local helper is reachable either when it is applied directly
            // (`name(`) or when it is passed as a bare function value in
            // argument position (`list.map(items, name)`, depth >= 1), where a
            // higher-order call will invoke it. Both call-graph edges are
            // followed so a forbidden call hidden behind a passed helper is not
            // silently missed by the gate.
            Token::Ident(name) if functions.contains_key(name) => {
                let applied = matches!(tokens.get(index + 1), Some(Token::OpenParen));
                if applied || depth >= 1 {
                    callees.push(name.clone());
                }
            }
            _ => {}
        }
        index += 1;
    }
    for callee in callees {
        walk(tokens, functions, &callee, visited, violations);
    }
}

/// Matches a qualified call against the forbidden vocabulary.
fn match_forbidden(qualifier: &str, member: &str) -> Option<&'static ForbiddenCall> {
    FORBIDDEN_CALLS
        .iter()
        .find(|call| call.qualifier == qualifier && call.member == member)
}

/// Maps every top-level `fn <name>(...) ... { <body> }` (with optional `pub`) to
/// its body's token range, mirroring the control-flow extractor's function
/// mapping so the two analyses agree on function boundaries.
fn map_functions(tokens: &[Token]) -> BTreeMap<String, FnBody> {
    let mut functions = BTreeMap::new();
    let mut index = 0;
    while index < tokens.len() {
        if matches!(&tokens[index], Token::Ident(word) if word == "fn") {
            if let Some(Token::Ident(name)) = tokens.get(index + 1) {
                if let Some(open) = find_open_brace(tokens, index + 2, tokens.len()) {
                    if let Some(close) = match_brace(tokens, open, tokens.len()) {
                        functions.insert(
                            name.clone(),
                            FnBody {
                                start: open + 1,
                                end: close,
                            },
                        );
                        index = close + 1;
                        continue;
                    }
                }
            }
        }
        index += 1;
    }
    functions
}

#[cfg(test)]
mod tests {
    use super::{DeterminismError, ViolationKind, analyze_determinism};

    #[test]
    fn clean_workflow_has_no_violations() -> Result<(), Box<dyn std::error::Error>> {
        let source = "import aion/workflow\n\
             pub fn run(input) {\n  \
             let assert Ok(at) = workflow.now()\n  \
             let assert Ok(seed) = workflow.random()\n  \
             workflow.run(wrappers.charge_activity(input))\n}\n";
        assert!(analyze_determinism(source, "run")?.is_empty());
        Ok(())
    }

    #[test]
    fn direct_wall_clock_call_is_flagged() -> Result<(), Box<dyn std::error::Error>> {
        let source = "pub fn run(input) {\n  \
             let now = erlang.system_time(1000)\n  \
             workflow.run(wrappers.charge_activity(input))\n}\n";
        let violations = analyze_determinism(source, "run")?;
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].call, "erlang.system_time");
        assert_eq!(violations[0].kind, ViolationKind::WallClock);
        assert_eq!(violations[0].function, "run");
        Ok(())
    }

    #[test]
    fn entropy_in_a_reachable_helper_is_flagged() -> Result<(), Box<dyn std::error::Error>> {
        let source = "pub fn run(input) {\n  \
             let id = make_id(input)\n  \
             workflow.run(wrappers.charge_activity(id))\n}\n\
             fn make_id(input) {\n  float.random()\n}\n";
        let violations = analyze_determinism(source, "run")?;
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].call, "float.random");
        assert_eq!(violations[0].kind, ViolationKind::Entropy);
        assert_eq!(violations[0].function, "make_id");
        Ok(())
    }

    #[test]
    fn entropy_in_a_helper_passed_as_a_value_is_flagged() -> Result<(), Box<dyn std::error::Error>>
    {
        // `tainted` is never applied directly — it is passed as a bare function
        // value to a higher-order call (`list.map`), which invokes it. A gate
        // that followed only direct `name(` calls would miss the entropy hiding
        // behind the passed helper; this is the soundness edge the depth-aware
        // walk closes.
        let source = "pub fn run(input) {\n  \
             let _ = list.map(input, tainted)\n  \
             workflow.run(wrappers.charge_activity(input))\n}\n\
             fn tainted(item) {\n  float.random()\n}\n";
        let violations = analyze_determinism(source, "run")?;
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].call, "float.random");
        assert_eq!(violations[0].kind, ViolationKind::Entropy);
        assert_eq!(violations[0].function, "tainted");
        Ok(())
    }

    #[test]
    fn unreachable_helper_violation_is_not_flagged() -> Result<(), Box<dyn std::error::Error>> {
        // `tainted` draws entropy but is never called from `run`: it is dead
        // relative to the workflow, so it must not be flagged.
        let source = "pub fn run(input) {\n  \
             workflow.run(wrappers.charge_activity(input))\n}\n\
             fn tainted(input) {\n  int.random()\n}\n";
        assert!(analyze_determinism(source, "run")?.is_empty());
        Ok(())
    }

    #[test]
    fn forbidden_word_inside_a_string_literal_is_not_flagged()
    -> Result<(), Box<dyn std::error::Error>> {
        // The tokeniser excludes string contents from matching, so a log message
        // mentioning the call is never a false positive.
        let source = "pub fn run(input) {\n  \
             log(\"erlang.system_time is forbidden here\")\n  \
             workflow.run(wrappers.charge_activity(input))\n}\n";
        assert!(analyze_determinism(source, "run")?.is_empty());
        Ok(())
    }

    #[test]
    fn missing_entry_function_is_a_loud_error() {
        let source = "fn helper() {\n  Nil\n}\n";
        let result = analyze_determinism(source, "run");
        assert_eq!(
            result,
            Err(DeterminismError::EntryFunctionNotFound {
                function: "run".to_owned(),
            })
        );
    }

    #[test]
    fn mutually_recursive_helpers_terminate() -> Result<(), Box<dyn std::error::Error>> {
        let source = "pub fn run(input) {\n  ping(input)\n}\n\
             fn ping(input) {\n  pong(input)\n}\n\
             fn pong(input) {\n  ping(input)\n  os.system_time(1)\n}\n";
        let violations = analyze_determinism(source, "run")?;
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].call, "os.system_time");
        Ok(())
    }

    #[test]
    fn multiple_distinct_calls_are_all_reported() -> Result<(), Box<dyn std::error::Error>> {
        let source = "pub fn run(input) {\n  \
             let a = os.system_time(1)\n  \
             let b = crypto.strong_rand_bytes(16)\n  \
             let c = erlang.unique_integer([])\n}\n";
        let violations = analyze_determinism(source, "run")?;
        let calls: Vec<&str> = violations.iter().map(|v| v.call.as_str()).collect();
        assert_eq!(
            calls,
            vec![
                "crypto.strong_rand_bytes",
                "erlang.unique_integer",
                "os.system_time",
            ]
        );
        Ok(())
    }
}
