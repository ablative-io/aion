//! Parallel child fan-out emission shared by strict and tolerant regions.

use super::context::Emitter;

/// Spawn every child, await handles in item order, and restore result order.
pub(super) fn emit_strict(
    emitter: &mut Emitter<'_>,
    spawn: &str,
    items: &str,
    var: &str,
    bind: &str,
) {
    emitter.line(&format!(
        "use awl_handles_reversed <- result.try(list.try_fold({items}, [], \
         fn(awl_acc, {var}) {{"
    ));
    emitter.indented(|this| {
        this.line(&format!(
            "use awl_handle <- result.try(workflow.spawn{spawn} |> awl_error.map_spawn_error)"
        ));
        this.line("Ok([awl_handle, ..awl_acc])");
    });
    emitter.line("}))");
    emitter.line(
        "use awl_results_reversed <- result.try(list.try_fold(\
         list.reverse(awl_handles_reversed), [], fn(awl_acc, awl_handle) {",
    );
    emitter.indented(|this| {
        this.line(
            "use awl_item <- result.try(child.await(awl_handle) |> awl_error.map_child_error)",
        );
        this.line("Ok([awl_item, ..awl_acc])");
    });
    emitter.line("}))");
    emitter.line(&format!("let {bind} = list.reverse(awl_results_reversed)"));
}

/// Tolerant child fan-out preserves one optional slot per input item.
pub(super) fn emit_tolerant(
    emitter: &mut Emitter<'_>,
    spawn: &str,
    items: &str,
    var: &str,
    bind: &str,
) {
    emitter.line(&format!(
        "let awl_handles_reversed = list.fold({items}, [], fn(awl_acc, {var}) {{"
    ));
    emitter.indented(|this| {
        this.line(&format!("case workflow.spawn{spawn} {{"));
        this.indented(|this| {
            this.line("Ok(awl_handle) -> [Some(awl_handle), ..awl_acc]");
            this.line("Error(_) -> [None, ..awl_acc]");
        });
        this.line("}");
    });
    emitter.line("})");
    emitter.line(
        "let awl_results_reversed = list.fold(list.reverse(awl_handles_reversed), [], \
         fn(awl_acc, awl_handle) {",
    );
    emitter.indented(|this| {
        this.line("case awl_handle {");
        this.indented(|this| {
            this.line("Some(awl_live) ->");
            this.indented(|this| {
                this.line("case child.await(awl_live) {");
                this.indented(|this| {
                    this.line("Ok(awl_item) -> [Some(awl_item), ..awl_acc]");
                    this.line("Error(_) -> [None, ..awl_acc]");
                });
                this.line("}");
            });
            this.line("None -> [None, ..awl_acc]");
        });
        this.line("}");
    });
    emitter.line("})");
    emitter.line(&format!("let {bind} = list.reverse(awl_results_reversed)"));
}
