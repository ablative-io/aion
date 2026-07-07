use std::fmt::Write as _;

use crate::{
    ActionDecl, BinaryOp, CallExpr, CallTarget, Document, DurationLiteral, DurationUnit, Expr,
    HandlerBlock, HandlerTerminal, RetrySpec, StepDecl, StepOp, TypeRef,
};

/// Emit a complete Gleam workflow module for a parsed AWL document.
#[must_use]
pub fn emit(document: &Document) -> String {
    Emitter::new(document).emit()
}

struct Emitter<'a> {
    document: &'a Document,
    out: String,
    indent: usize,
}

impl<'a> Emitter<'a> {
    fn new(document: &'a Document) -> Self {
        Self {
            document,
            out: String::new(),
            indent: 0,
        }
    }

    fn emit(mut self) -> String {
        self.header();
        self.types();
        self.definition();
        self.run();
        self.execute();
        self.activity_wrappers();
        self.signal_refs();
        self.codecs();
        self.out
    }

    fn header(&mut self) {
        if let Some(about) = &self.document.about {
            for line in wrap_doc(&about.text) {
                self.line(&format!("//// {line}"));
            }
        } else {
            self.line("//// Generated from an AWL document.");
        }
        self.blank();
        self.line("import aion/activity");
        self.line("import aion/codec.{type Codec}");
        self.line("import aion/duration");
        self.line("import aion/error");
        self.line("import aion/signal");
        self.line("import aion/workflow");
        self.line("import gleam/dynamic.{type Dynamic}");
        self.line("import gleam/dynamic/decode");
        self.line("import gleam/json");
        self.line("import gleam/list");
        self.line("import gleam/option.{type Option, None, Some}");
        self.blank();
    }

    fn types(&mut self) {
        self.line("pub type AwlError {");
        self.indented(|this| {
            this.line("DecodeInputFailed(String)");
            this.line("ActivityFailed(String)");
            this.line("SignalFailed(String)");
            this.line("ChildFailed(String)");
            this.line("TimerFailed(String)");
            this.line("TimedOut(String)");
            this.line("Failed");
        });
        self.line("}");
        self.blank();

        for name in self.external_named_types() {
            self.line(&format!("pub type {name} {{"));
            self.indented(|this| {
                let ctor = constructor(&name);
                this.line(&format!("{ctor}(value: String)"));
            });
            self.line("}");
            self.blank();
        }

        self.emit_type(
            &self.input_type_name(),
            self.document
                .inputs
                .iter()
                .map(|field| (field.name.as_str(), &field.ty)),
        );
        for decl in &self.document.types {
            self.emit_type(
                &decl.name,
                decl.fields
                    .iter()
                    .map(|field| (field.name.as_str(), &field.ty)),
            );
        }
    }

    fn emit_type<'b, I>(&mut self, name: &str, fields: I)
    where
        I: IntoIterator<Item = (&'b str, &'b TypeRef)>,
    {
        self.line(&format!("pub type {name} {{"));
        let ctor = constructor(name);
        self.indented(|this| {
            this.line(&format!("{ctor}("));
            this.indented(|this| {
                for (field_name, field_type) in fields {
                    let field_type = gleam_type(field_type);
                    this.line(&format!("{field_name}: {field_type},"));
                }
            });
            this.line(")");
        });
        self.line("}");
        self.blank();
    }

    fn definition(&mut self) {
        let input_type = self.input_type_name();
        let output_type = self.output_type_name();
        self.line("/// Typed definition binding the codecs to the execute function.");
        self.line(&format!(
            "pub fn definition() -> workflow.WorkflowDefinition({input_type}, {output_type}, AwlError) {{"
        ));
        self.indented(|this| {
            this.line("workflow.define(");
            this.indented(|this| {
                let workflow_name = &this.document.workflow.name;
                let input_codec = snake(&input_type);
                let output_codec = snake(&output_type);
                this.line(&format!("\"{workflow_name}\","));
                this.line(&format!("{input_codec}_codec(),"));
                this.line(&format!("{output_codec}_codec(),"));
                this.line("awl_error_codec(),");
                this.line("execute,");
            });
            this.line(")");
        });
        self.line("}");
        self.blank();
    }

    fn run(&mut self) {
        let input_type = self.input_type_name();
        let output_type = self.output_type_name();
        self.line("/// Engine entry point.");
        self.line("pub fn run(raw_input: Dynamic) -> Result(String, AwlError) {");
        self.indented(|this| {
            this.line("case decode.run(raw_input, decode.string) {");
            this.indented(|this| {
                this.line("Ok(raw_json) ->");
                this.indented(|this| {
                    let input_codec = snake(&input_type);
                    this.line(&format!("case {input_codec}_codec().decode(raw_json) {{"));
                    this.indented(|this| {
                        this.line("Ok(input) ->");
                        this.indented(|this| {
                            this.line("case execute(input) {");
                            this.indented(|this| {
                                let output_codec = snake(&output_type);
                                this.line(&format!(
                                    "Ok(result) -> Ok({output_codec}_codec().encode(result))"
                                ));
                                this.line("Error(workflow_error) -> Error(workflow_error)");
                            });
                            this.line("}");
                        });
                        this.line("Error(codec.DecodeError(reason: reason, path: _)) ->");
                        this.indented(|this| {
                            this.line("Error(DecodeInputFailed(\"failed to decode workflow input: \" <> reason))");
                        });
                    });
                    this.line("}");
                });
                this.line("Error(_) -> Error(DecodeInputFailed(\"workflow input payload was not a string\"))");
            });
            this.line("}");
        });
        self.line("}");
        self.blank();
    }

    fn execute(&mut self) {
        let input_type = self.input_type_name();
        let output_type = self.output_type_name();
        self.line("/// Workflow body generated from the AWL steps.");
        self.line(&format!(
            "pub fn execute(input: {input_type}) -> Result({output_type}, AwlError) {{"
        ));
        self.indented(|this| {
            for input in &this.document.inputs {
                let name = &input.name;
                this.line(&format!("let {name} = input.{name}"));
            }
            if !this.document.inputs.is_empty() {
                this.blank();
            }
            for step in &this.document.steps {
                this.emit_step(step);
            }
            let finish = expr(&this.document.finish);
            this.line(&format!("Ok({finish})"));
        });
        self.line("}");
        self.blank();
    }

    fn emit_step(&mut self, step: &StepDecl) {
        if let Some(about) = &step.about {
            for line in wrap_doc(&about.text) {
                self.line(&format!("// {line}"));
            }
        }
        if let Some(when) = &step.when {
            if let Some(name) = step.bind_as.as_ref().map(|bind| bind.name.as_str()) {
                let guard = expr(when);
                self.line(&format!("let {name} = case {guard} {{"));
                self.indented(|this| {
                    this.line("True -> {");
                    this.indented(|this| this.emit_step_result(step));
                    this.line("}");
                    this.line(&format!("False -> {name}"));
                });
                self.line("}");
            } else {
                let guard = expr(when);
                self.line(&format!("case {guard} {{"));
                self.indented(|this| {
                    this.line("True -> {");
                    this.indented(|this| this.emit_step_result(step));
                    this.line("}");
                    this.line("False -> Nil");
                });
                self.line("}");
            }
        } else if let Some(name) = step.bind_as.as_ref().map(|bind| bind.name.as_str()) {
            self.line(&format!("let assert Ok({name}) ="));
            self.indented(|this| this.emit_step_expr(step));
        } else {
            self.line("let assert Ok(_) =");
            self.indented(|this| this.emit_step_expr(step));
        }
        self.blank();
    }

    fn emit_step_result(&mut self, step: &StepDecl) {
        if let Some(name) = step.bind_as.as_ref().map(|bind| bind.name.as_str()) {
            self.line(&format!("let assert Ok({name}) ="));
            self.indented(|this| this.emit_step_expr(step));
            self.line(name);
        } else {
            self.line("let assert Ok(_) =");
            self.indented(|this| this.emit_step_expr(step));
            self.line("Nil");
        }
    }

    fn emit_step_expr(&mut self, step: &StepDecl) {
        if let Some(each) = &step.each {
            if let StepOp::Do(CallTarget::Action(call)) = &step.op {
                let items = expr(&each.in_expr);
                let item_name = &each.name;
                self.line(&format!("workflow.map({items}, fn({item_name}) {{"));
                self.indented(|this| {
                    let mut activity = String::new();
                    this.write_activity_value(&mut activity, call, step);
                    this.line(&activity);
                });
                self.line("}) |> map_activity_error");
            } else {
                self.line("Error(ActivityFailed(\"each is only supported for action calls\"))");
            }
            return;
        }

        let mut inner = String::new();
        match &step.op {
            StepOp::Do(target) => self.write_call_pipeline(&mut inner, target, step),
            StepOp::Wait { signal, .. } => {
                let _ = write!(
                    inner,
                    "workflow.receive({signal}_signal()) |> map_receive_error"
                );
            }
            StepOp::Sleep(duration) => {
                let duration = duration_expr(duration);
                let _ = write!(inner, "workflow.sleep({duration}) |> map_timer_error");
            }
        }

        if let Some(timeout) = &step.timeout {
            let duration = duration_expr(timeout);
            self.line(&format!(
                "case workflow.with_timeout(fn() {{ {inner} }}, {duration}) {{"
            ));
            self.indented(|this| {
                this.line("Ok(value) -> Ok(value)");
                this.line("Error(error.TimedOutError(_)) ->");
                this.indented(|this| {
                    if let Some(handler) = &step.on_timeout {
                        if let HandlerTerminal::Finish(_) = &handler.terminal {
                            let value = this.default_step_value(step);
                            this.line(&format!("Ok({value})"));
                        } else {
                            this.emit_handler(handler);
                        }
                    } else {
                        this.line("Error(TimedOut(\"step timed out\"))");
                    }
                });
                this.line("Error(error.InnerError(inner)) -> Error(inner)");
                this.line(
                    "Error(error.TimeoutEngineFailure(message)) -> Error(TimerFailed(message))",
                );
            });
            self.line("}");
        } else if let Some(handler) = &step.on_failure {
            self.line(&format!("case {inner} {{"));
            self.indented(|this| {
                this.line("Ok(value) -> Ok(value)");
                this.line("Error(_) ->");
                this.indented(|this| this.emit_handler(handler));
            });
            self.line("}");
        } else {
            self.line(&inner);
        }
    }

    fn default_step_value(&self, step: &StepDecl) -> String {
        match &step.op {
            StepOp::Do(CallTarget::Action(call)) => self.action(call).map_or_else(
                || "Nil".to_owned(),
                |action| self.default_value(&action.returns),
            ),
            StepOp::Do(CallTarget::Child { .. }) | StepOp::Sleep(_) => "Nil".to_owned(),
            StepOp::Wait { signal, .. } => self
                .document
                .signals
                .iter()
                .find(|decl| decl.name == *signal)
                .map_or_else(|| "Nil".to_owned(), |decl| self.default_value(&decl.ty)),
        }
    }

    fn default_value(&self, ty: &TypeRef) -> String {
        match ty {
            TypeRef::Named { name, .. } if name == "String" => "\"\"".to_owned(),
            TypeRef::Named { name, .. } if name == "Int" => "0".to_owned(),
            TypeRef::Named { name, .. } if name == "Float" => "0.0".to_owned(),
            TypeRef::Named { name, .. } if name == "Bool" => "False".to_owned(),
            TypeRef::Named { name, .. } if name == "Nil" => "Nil".to_owned(),
            TypeRef::Named { name, .. } => self.default_named_value(name),
            TypeRef::List { .. } => "[]".to_owned(),
            TypeRef::Option { .. } => "None".to_owned(),
        }
    }

    fn default_named_value(&self, name: &str) -> String {
        if let Some(decl) = self.document.types.iter().find(|decl| decl.name == name) {
            let fields = decl
                .fields
                .iter()
                .map(|field| {
                    let name = &field.name;
                    let value = self.default_value(&field.ty);
                    format!("{name}: {value}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            let ctor = constructor(name);
            format!("{ctor}({fields})")
        } else {
            let ctor = constructor(name);
            format!("{ctor}(value: \"\")")
        }
    }

    fn write_activity_value(&self, inner: &mut String, call: &CallExpr, step: &StepDecl) {
        inner.push_str(&call.name);
        inner.push_str("_activity(");
        for (index, arg) in call.args.iter().enumerate() {
            if index > 0 {
                inner.push_str(", ");
            }
            inner.push_str(&expr(arg));
        }
        inner.push(')');
        if let Some(retry) = step
            .retry
            .as_ref()
            .or_else(|| self.action(call).and_then(|a| a.retry.as_ref()))
        {
            inner.push_str(" |> activity.retry(");
            inner.push_str(&retry_policy(retry));
            inner.push(')');
        }
        if let Some(timeout) = step
            .timeout
            .as_ref()
            .or_else(|| self.action(call).and_then(|a| a.timeout.as_ref()))
        {
            inner.push_str(" |> activity.timeout(");
            inner.push_str(&duration_expr(timeout));
            inner.push(')');
        }
        if let Some(queue) = step
            .queue
            .as_ref()
            .or_else(|| self.action(call).and_then(|a| a.queue.as_ref()))
        {
            inner.push_str(" |> activity.task_queue(");
            inner.push_str(&string_lit(queue));
            inner.push(')');
        }
        if let Some(node) = step
            .node
            .as_ref()
            .or_else(|| self.action(call).and_then(|a| a.node.as_ref()))
        {
            inner.push_str(" |> activity.node(");
            inner.push_str(&string_lit(node));
            inner.push(')');
        }
    }

    fn write_call_pipeline(&self, inner: &mut String, target: &CallTarget, step: &StepDecl) {
        match target {
            CallTarget::Action(call) => {
                self.write_activity_value(inner, call, step);
                inner.push_str(" |> workflow.run |> map_activity_error");
            }
            CallTarget::Child {
                workflow: name,
                args,
                ..
            } => {
                let input = if args.len() == 1 {
                    expr(&args[0])
                } else {
                    "Nil".to_owned()
                };
                let snake_name = snake(name);
                let _ = write!(
                    inner,
                    "workflow.spawn_and_wait(\"{snake_name}\", {snake_name}.execute, {input}, {snake_name}.input_codec(), {snake_name}.output_codec(), {snake_name}.awl_error_codec()) |> map_child_error"
                );
            }
        }
    }

    fn emit_handler(&mut self, handler: &HandlerBlock) {
        self.line("{");
        self.indented(|this| {
            for target in &handler.actions {
                let mut inner = String::new();
                this.write_call_pipeline(&mut inner, target, &empty_step());
                this.line(&format!("let assert Ok(_) = {inner}"));
            }
            match &handler.terminal {
                HandlerTerminal::Finish(value) => {
                    let value = expr(value);
                    this.line(&format!("Ok({value})"));
                }
                HandlerTerminal::Fail(_) => this.line("Error(Failed)"),
            }
        });
        self.line("}");
    }

    fn activity_wrappers(&mut self) {
        for action in &self.document.actions {
            let action_type_name = pascal(&action.name);
            let input_name = format!("{action_type_name}Input");
            self.emit_type(
                &input_name,
                action
                    .params
                    .iter()
                    .map(|field| (field.name.as_str(), &field.ty)),
            );
            let action_name = &action.name;
            self.line(&format!("fn {action_name}_activity("));
            self.indented(|this| {
                for param in &action.params {
                    let name = &param.name;
                    let ty = gleam_type(&param.ty);
                    this.line(&format!("{name}: {ty},"));
                }
            });
            let return_type = gleam_type(&action.returns);
            self.line(&format!(
                ") -> activity.Activity({input_name}, {return_type}) {{"
            ));
            self.indented(|this| {
                this.line("activity.new(");
                this.indented(|this| {
                    let action_name = &action.name;
                    let ctor = constructor(&input_name);
                    this.line(&format!("\"{action_name}\","));
                    this.line(&format!("{ctor}("));
                    this.indented(|this| {
                        for param in &action.params {
                            let name = &param.name;
                            this.line(&format!("{name}: {name},"));
                        }
                    });
                    this.line("),");
                    let input_codec = snake(&input_name);
                    let return_codec = codec_name(&action.returns);
                    this.line(&format!("{input_codec}_codec(),"));
                    this.line(&format!("{return_codec}_codec(),"));
                    this.line("fn(_) { Error(error.terminal(\"activity body is provided by a worker\")) },");
                });
                this.line(")");
            });
            self.line("}");
            self.blank();
        }
    }

    fn signal_refs(&mut self) {
        for signal_decl in &self.document.signals {
            let signal_name = &signal_decl.name;
            let signal_type = gleam_type(&signal_decl.ty);
            self.line(&format!(
                "fn {signal_name}_signal() -> signal.SignalRef({signal_type}) {{"
            ));
            self.indented(|this| {
                let codec = codec_name(&signal_decl.ty);
                this.line(&format!("signal.new(\"{signal_name}\", {codec}_codec())"));
            });
            self.line("}");
            self.blank();
        }
    }

    fn codecs(&mut self) {
        self.line("fn awl_error_codec() -> Codec(AwlError) {");
        self.indented(|this| {
            this.line("codec.json_codec(awl_error_to_json, awl_error_decoder())");
        });
        self.line("}");
        self.blank();
        self.line("fn awl_error_to_json(error_value: AwlError) -> json.Json {");
        self.indented(|this| {
            this.line("case error_value {");
            this.indented(|this| {
                for variant in ["DecodeInputFailed", "ActivityFailed", "SignalFailed", "ChildFailed", "TimerFailed", "TimedOut"] {
                    this.line(&format!("{variant}(message) -> json.object([#(\"tag\", json.string(\"{variant}\")), #(\"message\", json.string(message))])"));
                }
                this.line("Failed -> json.object([#(\"tag\", json.string(\"Failed\"))])");
            });
            this.line("}");
        });
        self.line("}");
        self.blank();
        self.line("fn awl_error_decoder() -> decode.Decoder(AwlError) {");
        self.indented(|this| {
            this.line("use tag <- decode.field(\"tag\", decode.string)");
            this.line("case tag {");
            this.indented(|this| {
                for variant in [
                    "DecodeInputFailed",
                    "ActivityFailed",
                    "SignalFailed",
                    "ChildFailed",
                    "TimerFailed",
                    "TimedOut",
                ] {
                    this.line(&format!("\"{variant}\" -> {{"));
                    this.indented(|this| {
                        this.line("use message <- decode.field(\"message\", decode.string)");
                        this.line(&format!("decode.success({variant}(message))"));
                    });
                    this.line("}");
                }
                this.line("\"Failed\" -> decode.success(Failed)");
                this.line("_ -> decode.failure(Failed, \"AwlError\")");
            });
            this.line("}");
        });
        self.line("}");
        self.blank();

        for name in self.external_named_types() {
            self.emit_external_codec(&name);
        }
        self.emit_codec(
            &self.input_type_name(),
            self.document
                .inputs
                .iter()
                .map(|field| (field.name.as_str(), &field.ty)),
        );
        for decl in &self.document.types {
            self.emit_codec(
                &decl.name,
                decl.fields
                    .iter()
                    .map(|field| (field.name.as_str(), &field.ty)),
            );
        }
        for action in &self.document.actions {
            let action_type_name = pascal(&action.name);
            self.emit_codec(
                &format!("{action_type_name}Input"),
                action
                    .params
                    .iter()
                    .map(|field| (field.name.as_str(), &field.ty)),
            );
        }
        let mut composite_types = Vec::new();
        self.collect_composite_types(&mut composite_types);
        for ty in composite_types {
            self.emit_composite_codec(&ty);
        }
        self.builtin_codecs();
        self.error_mappers();
    }

    fn emit_external_codec(&mut self, name: &str) {
        let codec_fn = snake(name);
        let constructor_name = constructor(name);
        self.line(&format!("fn {codec_fn}_codec() -> Codec({name}) {{"));
        self.indented(|this| {
            this.line(&format!(
                "codec.json_codec({codec_fn}_to_json, {codec_fn}_decoder())"
            ));
        });
        self.line("}");
        self.line(&format!(
            "fn {codec_fn}_to_json(value: {name}) -> json.Json {{"
        ));
        self.indented(|this| {
            this.line("json.string(value.value)");
        });
        self.line("}");
        self.line(&format!(
            "fn {codec_fn}_decoder() -> decode.Decoder({name}) {{"
        ));
        self.indented(|this| {
            this.line("use value <- decode.then(decode.string)");
            this.line(&format!("decode.success({constructor_name}(value: value))"));
        });
        self.line("}");
        self.blank();
    }

    fn emit_codec<'b, I>(&mut self, name: &str, fields: I)
    where
        I: IntoIterator<Item = (&'b str, &'b TypeRef)>,
    {
        let fields = fields.into_iter().collect::<Vec<_>>();
        let codec_fn = snake(name);
        let value_var = snake(name);
        self.line(&format!("fn {codec_fn}_codec() -> Codec({name}) {{"));
        self.indented(|this| {
            this.line(&format!(
                "codec.json_codec({codec_fn}_to_json, {codec_fn}_decoder())"
            ));
        });
        self.line("}");
        self.blank();
        self.line(&format!(
            "fn {codec_fn}_to_json({value_var}: {name}) -> json.Json {{"
        ));
        self.indented(|this| {
            this.line("json.object([");
            this.indented(|this| {
                for (field_name, field_type) in &fields {
                    let codec = codec_name(field_type);
                    this.line(&format!(
                        "#(\"{field_name}\", {codec}_to_json({value_var}.{field_name})),"
                    ));
                }
            });
            this.line("])");
        });
        self.line("}");
        self.blank();
        self.line(&format!(
            "fn {codec_fn}_decoder() -> decode.Decoder({name}) {{"
        ));
        self.indented(|this| {
            for (field_name, field_type) in &fields {
                let codec = codec_name(field_type);
                this.line(&format!(
                    "use {field_name} <- decode.field(\"{field_name}\", {codec}_decoder())"
                ));
            }
            let ctor = constructor(name);
            this.line(&format!("decode.success({ctor}("));
            this.indented(|this| {
                for (field_name, _) in &fields {
                    this.line(&format!("{field_name}: {field_name},"));
                }
            });
            this.line("))");
        });
        self.line("}");
        self.blank();
    }

    fn collect_composite_types(&self, types: &mut Vec<TypeRef>) {
        for input in &self.document.inputs {
            collect_composite_type(&input.ty, types);
        }
        if let Some(output) = &self.document.output {
            collect_composite_type(&output.ty, types);
        }
        for signal_decl in &self.document.signals {
            collect_composite_type(&signal_decl.ty, types);
        }
        for decl in &self.document.types {
            for field in &decl.fields {
                collect_composite_type(&field.ty, types);
            }
        }
        for action in &self.document.actions {
            for param in &action.params {
                collect_composite_type(&param.ty, types);
            }
            collect_composite_type(&action.returns, types);
        }
    }

    fn emit_composite_codec(&mut self, ty: &TypeRef) {
        match ty {
            TypeRef::Named { .. } => {}
            TypeRef::List { inner, .. } => {
                let name = codec_name(ty);
                let inner_name = codec_name(inner);
                self.line(&format!(
                    "fn {name}_codec() -> Codec({}) {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!(
                        "codec.json_codec({name}_to_json, {name}_decoder())"
                    ));
                });
                self.line("}");
                self.line(&format!(
                    "fn {name}_to_json(values: {}) -> json.Json {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!("list_to_json(values, {inner_name}_to_json)"));
                });
                self.line("}");
                self.line(&format!(
                    "fn {name}_decoder() -> decode.Decoder({}) {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!("list_decoder({inner_name}_decoder())"));
                });
                self.line("}");
                self.blank();
            }
            TypeRef::Option { inner, .. } => {
                let name = codec_name(ty);
                let inner_name = codec_name(inner);
                self.line(&format!(
                    "fn {name}_codec() -> Codec({}) {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!(
                        "codec.json_codec({name}_to_json, {name}_decoder())"
                    ));
                });
                self.line("}");
                self.line(&format!(
                    "fn {name}_to_json(value: {}) -> json.Json {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!("option_to_json(value, {inner_name}_to_json)"));
                });
                self.line("}");
                self.line(&format!(
                    "fn {name}_decoder() -> decode.Decoder({}) {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!("option_decoder({inner_name}_decoder())"));
                });
                self.line("}");
                self.blank();
            }
        }
    }

    fn builtin_codecs(&mut self) {
        self.line(
            "fn string_codec() -> Codec(String) { codec.json_codec(json.string, decode.string) }",
        );
        self.line("fn int_codec() -> Codec(Int) { codec.json_codec(json.int, decode.int) }");
        self.line(
            "fn float_codec() -> Codec(Float) { codec.json_codec(json.float, decode.float) }",
        );
        self.line("fn bool_codec() -> Codec(Bool) { codec.json_codec(json.bool, decode.bool) }");
        self.line("fn nil_codec() -> Codec(Nil) { codec.json_codec(fn(_) { json.object([]) }, decode.success(Nil)) }");
        self.blank();
        self.line("fn string_to_json(value: String) -> json.Json { json.string(value) }");
        self.line("fn int_to_json(value: Int) -> json.Json { json.int(value) }");
        self.line("fn float_to_json(value: Float) -> json.Json { json.float(value) }");
        self.line("fn bool_to_json(value: Bool) -> json.Json { json.bool(value) }");
        self.line("fn nil_to_json(_: Nil) -> json.Json { json.object([]) }");
        self.blank();
        self.line("fn string_decoder() -> decode.Decoder(String) { decode.string }");
        self.line("fn int_decoder() -> decode.Decoder(Int) { decode.int }");
        self.line("fn float_decoder() -> decode.Decoder(Float) { decode.float }");
        self.line("fn bool_decoder() -> decode.Decoder(Bool) { decode.bool }");
        self.line("fn nil_decoder() -> decode.Decoder(Nil) { decode.success(Nil) }");
        self.blank();
        self.line("fn list_to_json(values: List(a), item_to_json: fn(a) -> json.Json) -> json.Json { json.array(values, item_to_json) }");
        self.line("fn list_decoder(item_decoder: decode.Decoder(a)) -> decode.Decoder(List(a)) { decode.list(item_decoder) }");
        self.line("fn option_to_json(value: Option(a), item_to_json: fn(a) -> json.Json) -> json.Json { json.nullable(value, item_to_json) }");
        self.line("fn option_decoder(item_decoder: decode.Decoder(a)) -> decode.Decoder(Option(a)) { decode.optional(item_decoder) }");
        self.blank();
    }

    fn error_mappers(&mut self) {
        self.line("fn try(result: Result(a, AwlError), next: fn(a) -> Result(b, AwlError)) -> Result(b, AwlError) {");
        self.indented(|this| {
            this.line(
                "case result { Ok(value) -> next(value) Error(awl_error) -> Error(awl_error) }",
            );
        });
        self.line("}");
        self.blank();
        self.line("fn map_activity_error(result: Result(a, error.ActivityError)) -> Result(a, AwlError) {");
        self.indented(|this| {
            this.line("case result { Ok(value) -> Ok(value) Error(_) -> Error(ActivityFailed(\"activity failed\")) }");
        });
        self.line("}");
        self.blank();
        self.line(
            "fn map_receive_error(result: Result(a, error.ReceiveError)) -> Result(a, AwlError) {",
        );
        self.indented(|this| {
            this.line("case result { Ok(value) -> Ok(value) Error(_) -> Error(SignalFailed(\"signal receive failed\")) }");
        });
        self.line("}");
        self.blank();
        self.line("fn map_child_error(result: Result(a, error.ChildError(AwlError))) -> Result(a, AwlError) {");
        self.indented(|this| {
            this.line("case result { Ok(value) -> Ok(value) Error(_) -> Error(ChildFailed(\"child workflow failed\")) }");
        });
        self.line("}");
        self.blank();
        self.line(
            "fn map_timer_error(result: Result(a, error.EngineError)) -> Result(a, AwlError) {",
        );
        self.indented(|this| {
            this.line("case result { Ok(value) -> Ok(value) Error(_) -> Error(TimerFailed(\"timer failed\")) }");
        });
        self.line("}");
        self.blank();
    }

    fn external_named_types(&self) -> Vec<String> {
        let declared = self
            .document
            .types
            .iter()
            .map(|decl| decl.name.as_str())
            .collect::<Vec<_>>();
        let mut names = Vec::new();
        self.collect_named_refs(&mut names);
        names
            .into_iter()
            .filter(|name| !is_builtin_type(name))
            .filter(|name| !declared.iter().any(|declared_name| declared_name == name))
            .collect()
    }

    fn collect_named_refs(&self, names: &mut Vec<String>) {
        for input in &self.document.inputs {
            collect_named_ref(&input.ty, names);
        }
        if let Some(output) = &self.document.output {
            collect_named_ref(&output.ty, names);
        }
        for signal_decl in &self.document.signals {
            collect_named_ref(&signal_decl.ty, names);
        }
        for decl in &self.document.types {
            for field in &decl.fields {
                collect_named_ref(&field.ty, names);
            }
        }
        for action in &self.document.actions {
            for param in &action.params {
                collect_named_ref(&param.ty, names);
            }
            collect_named_ref(&action.returns, names);
        }
    }

    fn action(&self, call: &CallExpr) -> Option<&ActionDecl> {
        self.document
            .actions
            .iter()
            .find(|action| action.name == call.name)
    }

    fn input_type_name(&self) -> String {
        let workflow_type_name = pascal(&self.document.workflow.name);
        format!("{workflow_type_name}Input")
    }

    fn output_type_name(&self) -> String {
        self.document
            .output
            .as_ref()
            .map_or_else(|| "Nil".to_owned(), |decl| gleam_type(&decl.ty))
    }

    fn line(&mut self, text: &str) {
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn blank(&mut self) {
        self.out.push('\n');
    }

    fn indented(&mut self, f: impl FnOnce(&mut Self)) {
        self.indent += 1;
        f(self);
        self.indent -= 1;
    }
}

fn empty_step() -> StepDecl {
    StepDecl {
        span: crate::Span {
            start: 0,
            end: 0,
            line: 0,
            column: 0,
        },
        trivia: crate::Trivia::default(),
        name: String::new(),
        about: None,
        when: None,
        each: None,
        op: StepOp::Sleep(DurationLiteral {
            span: crate::Span {
                start: 0,
                end: 0,
                line: 0,
                column: 0,
            },
            magnitude: 0,
            unit: DurationUnit::Seconds,
        }),
        repeat: None,
        until: None,
        retry: None,
        timeout: None,
        on_timeout: None,
        on_failure: None,
        bind_as: None,
        queue: None,
        node: None,
        leading_comments: Vec::new(),
        trailing_comments: Vec::new(),
    }
}

fn collect_named_ref(ty: &TypeRef, names: &mut Vec<String>) {
    match ty {
        TypeRef::Named { name, .. } => {
            if !names.iter().any(|seen| seen == name) {
                names.push(name.clone());
            }
        }
        TypeRef::List { inner, .. } | TypeRef::Option { inner, .. } => {
            collect_named_ref(inner, names);
        }
    }
}

fn is_builtin_type(name: &str) -> bool {
    matches!(name, "String" | "Int" | "Float" | "Bool" | "Nil")
}

fn collect_composite_type(ty: &TypeRef, types: &mut Vec<TypeRef>) {
    match ty {
        TypeRef::Named { .. } => {}
        TypeRef::List { inner, .. } | TypeRef::Option { inner, .. } => {
            collect_composite_type(inner, types);
            if !types.iter().any(|seen| codec_name(seen) == codec_name(ty)) {
                types.push(ty.clone());
            }
        }
    }
}

fn gleam_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Named { name, .. } => name.clone(),
        TypeRef::List { inner, .. } => {
            let inner = gleam_type(inner);
            format!("List({inner})")
        }
        TypeRef::Option { inner, .. } => {
            let inner = gleam_type(inner);
            format!("Option({inner})")
        }
    }
}

fn codec_name(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Named { name, .. } => snake(name),
        TypeRef::List { inner, .. } => {
            let inner = codec_name(inner);
            format!("list_{inner}")
        }
        TypeRef::Option { inner, .. } => {
            let inner = codec_name(inner);
            format!("option_{inner}")
        }
    }
}

fn expr(value: &Expr) -> String {
    match value {
        Expr::String { value, .. } => string_lit(value),
        Expr::Int { value, .. } => value.to_string(),
        Expr::Float { value, .. } => value.clone(),
        Expr::Bool { value, .. } => if *value { "True" } else { "False" }.to_owned(),
        Expr::Duration(duration) => duration_expr(duration),
        Expr::List { items, .. } => {
            let values = items.iter().map(expr).collect::<Vec<_>>().join(", ");
            format!("[{values}]")
        }
        Expr::Ref { name, .. } => name.clone(),
        Expr::Field { base, field, .. } => {
            let base = expr(base);
            format!("{base}.{field}")
        }
        Expr::Record { name, fields, .. } => {
            let fields = fields
                .iter()
                .map(|field| {
                    let name = &field.name;
                    let value = expr(&field.value);
                    format!("{name}: {value}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            let ctor = constructor(name);
            format!("{ctor}({fields})")
        }
        Expr::Not { expr: inner, .. } => {
            let inner = parenthesized(inner);
            format!("!{inner}")
        }
        Expr::Binary {
            left, op, right, ..
        } => {
            let left = parenthesized(left);
            let op = binary_op(*op);
            let right = parenthesized(right);
            format!("{left} {op} {right}")
        }
    }
}

fn parenthesized(value: &Expr) -> String {
    match value {
        Expr::String { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Bool { .. }
        | Expr::Duration(_)
        | Expr::List { .. }
        | Expr::Ref { .. }
        | Expr::Field { .. }
        | Expr::Record { .. } => expr(value),
        Expr::Not { .. } | Expr::Binary { .. } => {
            let value = expr(value);
            format!("({value})")
        }
    }
}

fn binary_op(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Or => "||",
        BinaryOp::And => "&&",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::Add => "<>",
    }
}

fn duration_expr(duration: &DurationLiteral) -> String {
    let milliseconds = match duration.unit {
        DurationUnit::Seconds => duration.magnitude.saturating_mul(1_000),
        DurationUnit::Minutes => duration.magnitude.saturating_mul(60_000),
        DurationUnit::Hours => duration.magnitude.saturating_mul(3_600_000),
        DurationUnit::Days => duration.magnitude.saturating_mul(86_400_000),
    };
    format!("duration.milliseconds({milliseconds})")
}

fn retry_policy(retry: &RetrySpec) -> String {
    match retry {
        RetrySpec::Every { count, every, .. } => format!(
            "activity.RetryPolicy(max_attempts: {}, backoff: activity.Fixed({}))",
            count,
            duration_expr(every)
        ),
        RetrySpec::Backoff {
            count, min, max, ..
        } => format!(
            "activity.RetryPolicy(max_attempts: {}, backoff: activity.Exponential(initial: {}, multiplier: 2.0, max: {}))",
            count,
            duration_expr(min),
            duration_expr(max)
        ),
    }
}

fn string_lit(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn constructor(name: &str) -> String {
    pascal(name)
}

fn pascal(name: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for character in name.chars() {
        if character == '_' {
            upper = true;
        } else if upper {
            out.extend(character.to_uppercase());
            upper = false;
        } else {
            out.push(character);
        }
    }
    out
}

fn snake(name: &str) -> String {
    let mut out = String::new();
    for (index, character) in name.chars().enumerate() {
        if character.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(character.to_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

fn wrap_doc(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    vec![text.to_owned()]
}
