use crate::TypeRef;

use super::context::Emitter;
use super::helpers::{constructor, gleam_type, ident, snake, wrap_doc};

impl Emitter<'_> {
    pub(super) fn header(&mut self) {
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

    pub(super) fn types(&mut self) {
        self.line("pub type AwlError {");
        self.indented(|this| {
            this.line("AwlDecodeInputFailed(String)");
            this.line("AwlActivityFailed(String)");
            this.line("AwlSignalFailed(String)");
            this.line("AwlChildFailed(String)");
            this.line("AwlTimerFailed(String)");
            this.line("AwlTimedOut(String)");
            this.line("AwlFailed");
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

    pub(super) fn emit_type<'b, I>(&mut self, name: &str, fields: I)
    where
        I: IntoIterator<Item = (&'b str, &'b TypeRef)>,
    {
        let fields = fields.into_iter().collect::<Vec<_>>();
        self.line(&format!("pub type {name} {{"));
        let ctor = constructor(name);
        self.indented(|this| {
            if fields.is_empty() {
                this.line(&ctor);
            } else {
                this.line(&format!("{ctor}("));
                this.indented(|this| {
                    for (field_name, field_type) in fields {
                        let field_name = ident(field_name);
                        let field_type = gleam_type(field_type);
                        this.line(&format!("{field_name}: {field_type},"));
                    }
                });
                this.line(")");
            }
        });
        self.line("}");
        self.blank();
    }

    pub(super) fn definition(&mut self) {
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

    pub(super) fn run(&mut self) {
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
                            this.line("Error(AwlDecodeInputFailed(\"failed to decode workflow input: \" <> reason))");
                        });
                    });
                    this.line("}");
                });
                this.line("Error(_) -> Error(AwlDecodeInputFailed(\"workflow input payload was not a string\"))");
            });
            this.line("}");
        });
        self.line("}");
        self.blank();
    }
}
