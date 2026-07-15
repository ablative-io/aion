//! Hygienic allocation for every generated local identifier.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{Document, ForkHeader, PipeEnd, Statement, Step};

use super::names::ident;

/// Persisted generated names chosen while shaping.
#[derive(Debug, Clone)]
pub(crate) struct GeneratedNames {
    pub(crate) counters: BTreeMap<usize, String>,
    pub(crate) allocator: NameAllocator,
}

impl GeneratedNames {
    pub(crate) fn new(document: &Document) -> Self {
        Self {
            counters: BTreeMap::new(),
            allocator: NameAllocator::for_document(document),
        }
    }

    pub(crate) fn counter(&self, step: &Step) -> Option<&str> {
        step.max_visits
            .as_ref()
            .and_then(|bound| self.counters.get(&bound.span.start))
            .map(String::as_str)
    }
}

/// One allocator seeded from all author-visible bindings in the document.
#[derive(Debug, Clone)]
pub(crate) struct NameAllocator {
    used: BTreeSet<String>,
}

impl NameAllocator {
    fn for_document(document: &Document) -> Self {
        let mut used = BTreeSet::new();
        for input in &document.inputs {
            used.insert(ident(&input.name));
        }
        collect_step_names(&document.steps, &mut used);
        for subflow in &document.subflows {
            for param in &subflow.params {
                used.insert(ident(&param.name));
            }
            collect_step_names(&subflow.steps, &mut used);
        }
        Self { used }
    }

    /// Reserve a deterministic candidate, suffixing from `_2` until free.
    pub(crate) fn fresh(&mut self, base: &str) -> String {
        let base = ident(base);
        if self.used.insert(base.clone()) {
            return base;
        }
        let mut suffix = 2usize;
        loop {
            let candidate = format!("{base}_{suffix}");
            if self.used.insert(candidate.clone()) {
                return candidate;
            }
            suffix += 1;
        }
    }
}

fn collect_step_names(steps: &[Step], names: &mut BTreeSet<String>) {
    for step in steps {
        collect_statement_names(&step.body, names);
        if let Some(on_failure) = &step.on_failure {
            collect_statement_names(&on_failure.body, names);
        }
    }
}

fn collect_statement_names(statements: &[Statement], names: &mut BTreeSet<String>) {
    for statement in statements {
        match statement {
            Statement::Call(call) => {
                if let Some(bind) = &call.bind {
                    names.insert(ident(&bind.name));
                }
            }
            Statement::Pipe(pipe) => {
                if let PipeEnd::Bind(bind) = &pipe.end {
                    names.insert(ident(&bind.name));
                }
            }
            Statement::Wait(wait) => {
                names.insert(ident(&wait.bind.name));
            }
            Statement::Fork(fork) => {
                if let ForkHeader::Collection { var, .. } = &fork.header {
                    names.insert(ident(var));
                }
                if let Some(bind) = &fork.join.bind {
                    names.insert(ident(&bind.name));
                }
                collect_statement_names(&fork.body, names);
            }
            Statement::Loop(looped) => {
                names.insert(ident(&looped.var));
                if let Some(counter) = &looped.counter {
                    names.insert(ident(&counter.name));
                }
                collect_statement_names(&looped.body, names);
            }
            Statement::SubStep(sub) => {
                collect_step_names(std::slice::from_ref(sub), names);
            }
            Statement::Distribute(distribute) => {
                names.insert(ident(&distribute.var));
            }
            Statement::Collect(collect) => {
                names.insert(ident(&collect.bind.name));
            }
            Statement::Spawn(_) | Statement::Sleep(_) | Statement::Route(_) => {}
        }
    }
}
