//! Parallel child fan-out emission shared by strict and tolerant regions.

use super::context::Emitter;

struct Names {
    handles: String,
    results: String,
    acc: String,
    handle: String,
    item: String,
    live: String,
}

fn names(emitter: &mut Emitter<'_>) -> Names {
    Names {
        handles: emitter.fresh_name("awl_handles_reversed"),
        results: emitter.fresh_name("awl_results_reversed"),
        acc: emitter.fresh_name("awl_acc"),
        handle: emitter.fresh_name("awl_handle"),
        item: emitter.fresh_name("awl_item"),
        live: emitter.fresh_name("awl_live"),
    }
}

/// Spawn every child, await handles in item order, and restore result order.
pub(super) fn emit_strict(
    emitter: &mut Emitter<'_>,
    spawn: &str,
    items: &str,
    var: &str,
    bind: &str,
) {
    let names = names(emitter);
    emitter.line(&format!(
        "use {} <- result.try(list.try_fold({items}, [], fn({}, {var}) {{",
        names.handles, names.acc
    ));
    emitter.indented(|this| {
        this.line(&format!(
            "use {} <- result.try(workflow.spawn{spawn} |> awl_error.map_spawn_error)",
            names.handle
        ));
        this.line(&format!("Ok([{}, ..{}])", names.handle, names.acc));
    });
    emitter.line("}))");
    emitter.line(&format!(
        "use {} <- result.try(list.try_fold(list.reverse({}), [], fn({}, {}) {{",
        names.results, names.handles, names.acc, names.handle
    ));
    emitter.indented(|this| {
        this.line(&format!(
            "use {} <- result.try(child.await({}) |> awl_error.map_child_error)",
            names.item, names.handle
        ));
        this.line(&format!("Ok([{}, ..{}])", names.item, names.acc));
    });
    emitter.line("}))");
    emitter.line(&format!("let {bind} = list.reverse({})", names.results));
}

/// Tolerant child fan-out preserves one optional slot per input item.
pub(super) fn emit_tolerant(
    emitter: &mut Emitter<'_>,
    spawn: &str,
    items: &str,
    var: &str,
    bind: &str,
) {
    let names = names(emitter);
    emitter.line(&format!(
        "let {} = list.fold({items}, [], fn({}, {var}) {{",
        names.handles, names.acc
    ));
    emitter.indented(|this| {
        this.line(&format!("case workflow.spawn{spawn} {{"));
        this.indented(|this| {
            this.line(&format!(
                "Ok({}) -> [Some({}), ..{}]",
                names.handle, names.handle, names.acc
            ));
            this.line(&format!("Error(_) -> [None, ..{}]", names.acc));
        });
        this.line("}");
    });
    emitter.line("})");
    emitter.line(&format!(
        "let {} = list.fold(list.reverse({}), [], fn({}, {}) {{",
        names.results, names.handles, names.acc, names.handle
    ));
    emitter.indented(|this| {
        this.line(&format!("case {} {{", names.handle));
        this.indented(|this| {
            this.line(&format!("Some({}) ->", names.live));
            this.indented(|this| {
                this.line(&format!("case child.await({}) {{", names.live));
                this.indented(|this| {
                    this.line(&format!(
                        "Ok({}) -> [Some({}), ..{}]",
                        names.item, names.item, names.acc
                    ));
                    this.line(&format!("Error(_) -> [None, ..{}]", names.acc));
                });
                this.line("}");
            });
            this.line(&format!("None -> [None, ..{}]", names.acc));
        });
        this.line("}");
    });
    emitter.line("})");
    emitter.line(&format!("let {bind} = list.reverse({})", names.results));
}
