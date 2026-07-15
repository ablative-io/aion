//! Activity wrappers (one per declared action, task queue baked in at the
//! call site) and signal references. The error mappers, indexing, and the
//! raw/decoded/child-input codecs are hoisted into the `aion/awl` SDK modules
//! (AWL-BC-0) and referenced qualified from the generated call sites.

use super::context::Emitter;
use super::frame::emit_record_type;
use super::names::{ident, snake};
use super::types::type_ref_to_g;

pub(super) fn activity_wrappers(emitter: &mut Emitter<'_>) {
    let workers: Vec<usize> = (0..emitter.document.workers.len()).collect();
    for worker_index in workers {
        let action_count = emitter.document.workers[worker_index].actions.len();
        for action_index in 0..action_count {
            let action = &emitter.document.workers[worker_index].actions[action_index];
            let action_name = action.name.clone();
            let docs: Vec<String> = action
                .docs
                .iter()
                .map(|line| line.text.strip_prefix(' ').unwrap_or(&line.text).to_owned())
                .collect();
            let params: Vec<(String, super::types::GType)> = action
                .params
                .iter()
                .map(|param| (param.name.clone(), type_ref_to_g(&param.ty)))
                .collect();
            let returns = type_ref_to_g(&action.returns);
            let input_name = emitter
                .action_inputs
                .get(&action_name)
                .cloned()
                .unwrap_or_else(|| format!("{}Input", super::names::pascal(&action_name)));

            emit_record_type(emitter, &input_name, &params);

            for doc in &docs {
                emitter.line(&format!("/// {doc}"));
            }
            let return_type = emitter.env.gleam_type(&returns);
            let wrapper = format!("{}_activity", snake(&action_name));
            if params.is_empty() {
                emitter.line(&format!(
                    "fn {wrapper}() -> activity.Activity({input_name}, {return_type}) {{"
                ));
            } else {
                emitter.line(&format!("fn {wrapper}("));
                emitter.indented(|this| {
                    for (name, ty) in &params {
                        let rendered = this.env.gleam_type(ty);
                        this.line(&format!("{}: {rendered},", ident(name)));
                    }
                });
                emitter.line(&format!(
                    ") -> activity.Activity({input_name}, {return_type}) {{"
                ));
            }
            emitter.indented(|this| {
                this.line("activity.new(");
                this.indented(|this| {
                    this.line(&format!("\"{action_name}\","));
                    if params.is_empty() {
                        this.line(&format!("{input_name},"));
                    } else {
                        this.line(&format!("{input_name}("));
                        this.indented(|this| {
                            for (name, _) in &params {
                                let rendered = ident(name);
                                this.line(&format!("{rendered}: {rendered},"));
                            }
                        });
                        this.line("),");
                    }
                    let input_codec = snake(&input_name);
                    let return_codec = this.codec_fn(&returns);
                    this.line(&format!("{input_codec}_codec(),"));
                    this.line(&format!("{return_codec}(),"));
                    this.line(
                        "fn(_) { Error(error.terminal(\"activity body is provided by a \
                         worker\")) },",
                    );
                });
                this.line(")");
            });
            emitter.line("}");
            emitter.blank();

            if emitter.flags.raw_actions.contains(&action_name) {
                raw_wrapper(emitter, &action_name, &params, &input_name);
            }
        }
    }
}

/// The raw twin of an action's activity wrapper: the same action name and
/// the same wire bytes (the input record is encoded with the action's own
/// input codec), but typed `Activity(String, String)` so differently-typed
/// parallel branches can share one `workflow.all` list. The join decodes
/// each branch's payload with its action's return codec (`awl_decoded`).
fn raw_wrapper(
    emitter: &mut Emitter<'_>,
    action_name: &str,
    params: &[(String, super::types::GType)],
    input_name: &str,
) {
    let wrapper = format!("{}_activity_raw", snake(action_name));
    if params.is_empty() {
        emitter.line(&format!(
            "fn {wrapper}() -> activity.Activity(String, String) {{"
        ));
    } else {
        emitter.line(&format!("fn {wrapper}("));
        emitter.indented(|this| {
            for (name, ty) in params {
                let rendered = this.env.gleam_type(ty);
                this.line(&format!("{}: {rendered},", ident(name)));
            }
        });
        emitter.line(") -> activity.Activity(String, String) {");
    }
    let input_codec = snake(input_name);
    let codec_local = emitter.fresh_name("awl_input_codec");
    emitter.indented(|this| {
        this.line(&format!("let {codec_local} = {input_codec}_codec()"));
        this.line("activity.new(");
        this.indented(|this| {
            this.line(&format!("\"{action_name}\","));
            if params.is_empty() {
                this.line(&format!("{codec_local}.encode({input_name}),"));
            } else {
                this.line(&format!("{codec_local}.encode({input_name}("));
                this.indented(|this| {
                    for (name, _) in params {
                        let rendered = ident(name);
                        this.line(&format!("{rendered}: {rendered},"));
                    }
                });
                this.line(")),");
            }
            this.line("awlc.raw(),");
            this.line("awlc.raw(),");
            this.line(
                "fn(_) { Error(error.terminal(\"activity body is provided by a worker\")) },",
            );
        });
        this.line(")");
    });
    emitter.line("}");
    emitter.blank();
}

pub(super) fn signal_refs(emitter: &mut Emitter<'_>) {
    for signal_index in 0..emitter.document.signals.len() {
        let signal = &emitter.document.signals[signal_index];
        let signal_name = signal.name.clone();
        let payload = type_ref_to_g(&signal.ty);
        let signal_type = emitter.env.gleam_type(&payload);
        let codec = emitter.codec_fn(&payload);
        emitter.line(&format!(
            "fn {}_signal() -> signal.SignalRef({signal_type}) {{",
            snake(&signal_name)
        ));
        emitter.indented(|this| {
            this.line(&format!("signal.new(\"{signal_name}\", {codec}())"));
        });
        emitter.line("}");
        emitter.blank();
    }
}
