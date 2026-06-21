//! Lightweight workflow facts the test-scaffold generator needs from source.
//!
//! `aion generate` emits an `aion/testing` skeleton per workflow that drives the
//! workflow's *typed* entry function and advances the simulated clock once per
//! durable timer. Neither fact is in the package manifest: the manifest's
//! `entry_function` is the engine-facing `run(raw_input: Dynamic)` adapter, while
//! the harness drives the typed `execute` passed to `workflow.define`; and the
//! timer count is a property of the workflow's control flow, not its
//! declarations.
//!
//! This module reads both from the entry-module source, reusing the same
//! [`super::scan`] tokeniser and the same function-mapping plus
//! reachability-over-local-calls the extractor and the determinism analyser use,
//! so all three agree on "reachable from workflow code". It is deliberately not a
//! Gleam type-checker: the typed entry is read as the last identifier argument of
//! the `workflow.define(...)` call, and timers are counted as the
//! `<alias>.sleep` / `<alias>.start_timer` primitive calls reachable from that
//! entry function — the same fixed vocabulary the graph extractor recognises.

use std::collections::{BTreeMap, BTreeSet};

use super::reader::{end_of_call, find_open_brace, match_brace};
use super::scan::{Token, tokenise};

/// The facts the test-scaffold generator derives from a workflow's source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowFacts {
    /// The typed entry function the test harness drives — the last argument of
    /// the `workflow.define(...)` call (e.g. `execute`).
    pub typed_entry_function: String,
    /// The number of durable timers (`sleep` / `start_timer`) reachable from the
    /// typed entry function — one clock advance is scaffolded per timer.
    pub timer_count: usize,
}

/// Errors raised while reading workflow facts from source.
#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum FactsError {
    /// The entry-module source never imports `aion/workflow`, so it composes
    /// none of the recognised primitives and is not a workflow the scaffold
    /// generator understands.
    #[error(
        "workflow source does not import `aion/workflow`; the test-scaffold generator only \
         understands workflows that compose the `aion/workflow` primitive vocabulary"
    )]
    NoWorkflowImport,

    /// No `workflow.define(...)` call was found, so the typed entry function the
    /// harness must drive cannot be identified.
    #[error(
        "workflow source contains no `<alias>.define(...)` call; the typed entry function the \
         test scaffold drives is the last argument of that call"
    )]
    NoDefineCall,

    /// A `workflow.define(...)` call was found but its last argument is not a
    /// bare function reference, so the typed entry function cannot be named.
    #[error(
        "the `<alias>.define(...)` call's last argument is not a bare entry-function reference; \
         the test scaffold cannot identify the typed entry function to drive"
    )]
    EntryFunctionNotIdentifiable,
}

/// A function body as the half-open token range strictly inside its `{ }`.
#[derive(Clone, Copy)]
struct FnBody {
    start: usize,
    end: usize,
}

/// Reads the [`WorkflowFacts`] from a workflow's entry-module Gleam `source`.
///
/// # Errors
///
/// Returns [`FactsError::NoWorkflowImport`] when the source does not import
/// `aion/workflow`, [`FactsError::NoDefineCall`] when no `<alias>.define(...)`
/// call is present, and [`FactsError::EntryFunctionNotIdentifiable`] when that
/// call's last argument is not a bare function reference.
pub fn extract_workflow_facts(source: &str) -> Result<WorkflowFacts, FactsError> {
    let tokens = tokenise(source);
    let alias = workflow_alias(&tokens).ok_or(FactsError::NoWorkflowImport)?;
    let typed_entry_function = define_entry_function(&tokens, &alias)?;

    let functions = map_functions(&tokens);
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let timer_count = if functions.contains_key(&typed_entry_function) {
        count_timers(
            &tokens,
            &functions,
            &alias,
            &typed_entry_function,
            &mut visited,
        )
    } else {
        // The define call named a function the mapper did not find (e.g. an
        // imported entry). With no body to walk, no timers are attributable;
        // report zero rather than guessing.
        0
    };

    Ok(WorkflowFacts {
        typed_entry_function,
        timer_count,
    })
}

/// Finds the alias the source imports `aion/workflow` under (`workflow` by
/// default, or the `as` alias), or `None` when it does not import it.
fn workflow_alias(tokens: &[Token]) -> Option<String> {
    let mut index = 0;
    while index < tokens.len() {
        if matches!(&tokens[index], Token::Ident(word) if word == "import")
            && import_path_is_workflow(tokens, index + 1)
        {
            let after_path = index + 1 + 3;
            if matches!(tokens.get(after_path), Some(Token::Ident(word)) if word == "as") {
                if let Some(Token::Ident(alias)) = tokens.get(after_path + 1) {
                    return Some(alias.clone());
                }
            }
            return Some("workflow".to_owned());
        }
        index += 1;
    }
    None
}

/// Whether the import-path tokens beginning at `start` spell `aion/workflow`
/// (and not a deeper submodule like `aion/workflow/timer`).
fn import_path_is_workflow(tokens: &[Token], start: usize) -> bool {
    matches!(
        (tokens.get(start), tokens.get(start + 1), tokens.get(start + 2)),
        (
            Some(Token::Ident(first)),
            Some(Token::Other('/')),
            Some(Token::Ident(second)),
        ) if first == "aion"
            && second == "workflow"
            && !matches!(tokens.get(start + 3), Some(Token::Other('/')))
    )
}

/// Reads the typed entry function from the first `<alias>.define(...)` call: it
/// is the last bare identifier argument before the call's closing paren.
fn define_entry_function(tokens: &[Token], alias: &str) -> Result<String, FactsError> {
    let mut index = 0;
    while index < tokens.len() {
        if let Token::Qualified { left, right } = &tokens[index] {
            if left == alias && right == "define" {
                let end = end_of_call(tokens, index, tokens.len());
                return last_identifier_argument(tokens, index + 1, end)
                    .ok_or(FactsError::EntryFunctionNotIdentifiable);
            }
        }
        index += 1;
    }
    Err(FactsError::NoDefineCall)
}

/// Returns the last top-level bare-identifier argument in the call's argument
/// range `[start, end)` — the entry function passed to `workflow.define`. Only
/// identifiers at the call's own paren depth are considered, so a nested call's
/// arguments are never mistaken for the entry function.
fn last_identifier_argument(tokens: &[Token], start: usize, end: usize) -> Option<String> {
    let upper = end.min(tokens.len());
    let mut depth = 0_i32;
    let mut last: Option<String> = None;
    for token in tokens.iter().take(upper).skip(start) {
        match token {
            Token::OpenParen => depth += 1,
            Token::CloseParen => depth -= 1,
            // A bare identifier directly inside the define call's own parens
            // (depth 1) is a candidate entry function; the last one wins.
            Token::Ident(word) if depth == 1 => last = Some(word.clone()),
            // A qualified/other argument at depth 1 clears the candidate: the
            // entry function is a bare reference, not a qualified expression.
            Token::Qualified { .. } if depth == 1 => last = None,
            _ => {}
        }
    }
    last
}

/// Counts the durable timers (`<alias>.sleep` / `<alias>.start_timer`) reachable
/// from `function`, recursing into every reachable local helper exactly once.
fn count_timers(
    tokens: &[Token],
    functions: &BTreeMap<String, FnBody>,
    alias: &str,
    function: &str,
    visited: &mut BTreeSet<String>,
) -> usize {
    if !visited.insert(function.to_owned()) {
        return 0;
    }
    let Some(body) = functions.get(function).copied() else {
        return 0;
    };
    let mut count = 0;
    let mut callees: Vec<String> = Vec::new();
    let upper = body.end.min(tokens.len());
    let mut index = body.start;
    let mut depth: usize = 0;
    while index < upper {
        match &tokens[index] {
            Token::OpenParen => depth += 1,
            Token::CloseParen => depth = depth.saturating_sub(1),
            Token::Qualified { left, right }
                if left == alias && (right == "sleep" || right == "start_timer") =>
            {
                count += 1;
            }
            // Follow a helper both when applied directly (`name(`) and when
            // passed as a bare function value in argument position
            // (`list.map(items, name)`, depth >= 1), mirroring the determinism
            // walk so the timer count does not undercount a timer reached only
            // through a higher-order pass.
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
        count += count_timers(tokens, functions, alias, &callee, visited);
    }
    count
}

/// Maps every top-level `fn <name>(...) { <body> }` (with optional `pub`) to its
/// body's token range, mirroring the extractor's function mapping.
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
    use super::{FactsError, extract_workflow_facts};

    const SAGA: &str = "import aion/workflow\n\
         pub fn definition() {\n  \
         workflow.define(\"order\", a_codec(), b_codec(), c_codec(), execute)\n}\n\
         pub fn execute(input) {\n  \
         let _ = workflow.run(wrappers.charge_activity(input))\n  \
         let _ = workflow.sleep(duration.seconds(1))\n  \
         settle(input)\n}\n\
         fn settle(input) {\n  \
         workflow.start_timer(\"deadline\", duration.seconds(5))\n}\n";

    #[test]
    fn reads_typed_entry_and_counts_reachable_timers() -> Result<(), Box<dyn std::error::Error>> {
        let facts = extract_workflow_facts(SAGA)?;
        assert_eq!(facts.typed_entry_function, "execute");
        // `workflow.sleep` in execute plus `workflow.start_timer` in the reachable
        // `settle` helper: two durable timers.
        assert_eq!(facts.timer_count, 2);
        Ok(())
    }

    #[test]
    fn unreachable_timer_is_not_counted() -> Result<(), Box<dyn std::error::Error>> {
        let source = "import aion/workflow\n\
             pub fn definition() {\n  \
             workflow.define(\"f\", a(), b(), c(), execute)\n}\n\
             pub fn execute(input) {\n  \
             workflow.run(wrappers.charge_activity(input))\n}\n\
             fn dead(input) {\n  workflow.sleep(duration.seconds(1))\n}\n";
        let facts = extract_workflow_facts(source)?;
        assert_eq!(facts.timer_count, 0);
        Ok(())
    }

    #[test]
    fn timer_in_a_helper_passed_as_a_value_is_counted() -> Result<(), Box<dyn std::error::Error>> {
        // `delayed` is never applied directly — it is passed as a bare function
        // value to `list.map`, which invokes it. Counting its timer requires
        // following the passed helper, the same soundness edge the determinism
        // walk closes; a direct-call-only walk would undercount to zero.
        let source = "import aion/workflow\n\
             pub fn definition() {\n  \
             workflow.define(\"f\", a(), b(), c(), execute)\n}\n\
             pub fn execute(input) {\n  \
             let _ = list.map(input, delayed)\n  \
             workflow.run(wrappers.charge_activity(input))\n}\n\
             fn delayed(item) {\n  workflow.sleep(duration.seconds(1))\n}\n";
        let facts = extract_workflow_facts(source)?;
        assert_eq!(facts.typed_entry_function, "execute");
        assert_eq!(facts.timer_count, 1);
        Ok(())
    }

    #[test]
    fn aliased_workflow_import_is_honoured() -> Result<(), Box<dyn std::error::Error>> {
        let source = "import aion/workflow as wf\n\
             pub fn definition() {\n  \
             wf.define(\"f\", a(), b(), c(), execute)\n}\n\
             pub fn execute(input) {\n  wf.sleep(duration.seconds(1))\n}\n";
        let facts = extract_workflow_facts(source)?;
        assert_eq!(facts.typed_entry_function, "execute");
        assert_eq!(facts.timer_count, 1);
        Ok(())
    }

    #[test]
    fn missing_workflow_import_is_an_error() {
        let source = "pub fn execute(input) {\n  Nil\n}\n";
        assert_eq!(
            extract_workflow_facts(source),
            Err(FactsError::NoWorkflowImport)
        );
    }

    #[test]
    fn missing_define_call_is_an_error() {
        let source = "import aion/workflow\n\
             pub fn execute(input) {\n  workflow.run(wrappers.charge_activity(input))\n}\n";
        assert_eq!(
            extract_workflow_facts(source),
            Err(FactsError::NoDefineCall)
        );
    }
}
