use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write};

use aion_awl::{ActionDecl, Document, TypeBody, TypeDecl, TypeRef, WorkerDecl};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct ScaffoldRequest {
    pub source: String,
    pub worker: String,
    pub runtime: String,
}

#[derive(Debug, Serialize)]
pub struct ScaffoldResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<ScaffoldRefusal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ScaffoldRefusal {
    InvalidDocument {
        reason: String,
    },
    UnknownWorker {
        worker: String,
    },
    UnsupportedRuntime {
        runtime: String,
    },
    UnscaffoldableActionType {
        action: String,
        r#type: String,
        reason: String,
    },
}

impl ScaffoldResponse {
    fn generated(files: BTreeMap<String, String>) -> Self {
        Self {
            ok: true,
            files: Some(files),
            refusal: None,
        }
    }

    fn refused(refusal: ScaffoldRefusal) -> Self {
        Self {
            ok: false,
            files: None,
            refusal: Some(refusal),
        }
    }
}

pub fn scaffold(request: &ScaffoldRequest) -> ScaffoldResponse {
    let document = match aion_awl::parse(&request.source) {
        Ok(document) => document,
        Err(error) => {
            return ScaffoldResponse::refused(ScaffoldRefusal::InvalidDocument {
                reason: error.to_string(),
            });
        }
    };
    if let Some(error) = aion_awl::check(&document).first() {
        return ScaffoldResponse::refused(ScaffoldRefusal::InvalidDocument {
            reason: error.message.clone(),
        });
    }
    let Some(worker) = document
        .workers
        .iter()
        .find(|item| item.name == request.worker)
    else {
        return ScaffoldResponse::refused(ScaffoldRefusal::UnknownWorker {
            worker: request.worker.clone(),
        });
    };
    let result = match request.runtime.as_str() {
        "shell" => shell_files(&document, worker),
        "rust" => rust_files(&document, worker),
        _ => {
            return ScaffoldResponse::refused(ScaffoldRefusal::UnsupportedRuntime {
                runtime: request.runtime.clone(),
            });
        }
    };
    match result {
        Ok(files) => ScaffoldResponse::generated(files),
        Err(refusal) => ScaffoldResponse::refused(refusal),
    }
}

fn shell_files(
    document: &Document,
    worker: &WorkerDecl,
) -> Result<BTreeMap<String, String>, ScaffoldRefusal> {
    validate_reachable_types(document, worker)?;
    let mut manifest = format!(
        "# Generated wiring. Types, timeout, and retry live only in the .awl.\n\
         [worker]\nname = \"{}\"\ntask_queue = \"{}\"\n",
        worker.name, worker.name
    );
    for action in &worker.actions {
        let result = if is_string(&action.returns) {
            "text"
        } else {
            "json"
        };
        append(
            &mut manifest,
            format_args!(
                "\n[[action]]\nname = \"{}\"\ncommand = [\"printf\", \"%s\", \"{{input}}\"]\nresult = \"{result}\"\n",
                action.name
            ),
        );
    }
    let readme = "Run: `aion worker shell --manifest worker.toml --endpoint <server>`\n\nResult encoding is derived wiring; the type contract lives in the .awl and is enforced by the workflow's decoder, as for every worker.\n";
    Ok(BTreeMap::from([
        ("worker.toml".to_owned(), manifest),
        ("README.md".to_owned(), readme.to_owned()),
    ]))
}

fn rust_files(
    document: &Document,
    worker: &WorkerDecl,
) -> Result<BTreeMap<String, String>, ScaffoldRefusal> {
    let reachable = reachable_types(document, worker)?;
    let package = format!("{}-worker", worker.name.replace('_', "-"));
    let cargo = format!(
        "[package]\nname = \"{package}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
         [dependencies]\naion-worker = \"0.8.0\"\nserde = {{ version = \"1\", features = [\"derive\"] }}\n\
         tokio = {{ version = \"1\", features = [\"macros\", \"rt-multi-thread\"] }}\n"
    );
    let mut source = String::from(
        "use std::time::Duration;\n\nuse aion_worker::{ActivityContext, HandlerFuture, Worker, WorkerConfig};\nuse serde::{Deserialize, Serialize};\n\n",
    );
    for declaration in document
        .types
        .iter()
        .filter(|item| reachable.contains(&item.name))
    {
        emit_type(&mut source, declaration);
    }
    for action in &worker.actions {
        emit_action(&mut source, action);
    }
    source
        .push_str("#[tokio::main]\nasync fn main() -> Result<(), Box<dyn std::error::Error>> {\n");
    append(
        &mut source,
        format_args!(
            "    let config = WorkerConfig::builder()\n        .endpoint(\"http://127.0.0.1:50051\")\n        .task_queue(\"{}\")\n        .identity(\"{}-worker-1\")\n        .max_concurrency(4)\n        .reconnect_initial_backoff(Duration::from_millis(100))\n        .reconnect_max_backoff(Duration::from_secs(5))\n        .reconnect_max_attempts(10)\n        .build()?;\n\n    Worker::builder(config)\n",
            worker.name, worker.name
        ),
    );
    for action in &worker.actions {
        append(
            &mut source,
            format_args!(
                "        .register_activity(\"{}\", {})?\n",
                action.name,
                rust_ident(&action.name)
            ),
        );
    }
    source.push_str("        .build()?\n        .run()\n        .await?;\n    Ok(())\n}\n");
    Ok(BTreeMap::from([
        ("Cargo.toml".to_owned(), cargo),
        ("src/main.rs".to_owned(), source),
    ]))
}

fn validate_reachable_types(
    document: &Document,
    worker: &WorkerDecl,
) -> Result<(), ScaffoldRefusal> {
    reachable_types(document, worker).map(|_| ())
}

fn reachable_types(
    document: &Document,
    worker: &WorkerDecl,
) -> Result<BTreeSet<String>, ScaffoldRefusal> {
    let declarations: BTreeMap<&str, &TypeDecl> = document
        .types
        .iter()
        .map(|item| (item.name.as_str(), item))
        .collect();
    let mut reached = BTreeSet::new();
    for action in &worker.actions {
        for ty in action
            .params
            .iter()
            .map(|item| &item.ty)
            .chain(std::iter::once(&action.returns))
        {
            visit_type(ty, action, &declarations, &mut reached)?;
        }
    }
    Ok(reached)
}

fn visit_type(
    ty: &TypeRef,
    action: &ActionDecl,
    declarations: &BTreeMap<&str, &TypeDecl>,
    reached: &mut BTreeSet<String>,
) -> Result<(), ScaffoldRefusal> {
    match ty {
        TypeRef::List { inner, .. } | TypeRef::Optional { inner, .. } => {
            visit_type(inner, action, declarations, reached)
        }
        TypeRef::Named { name, .. } if is_builtin(name) => Ok(()),
        TypeRef::Named { name, .. } => {
            if !reached.insert(name.clone()) {
                return Ok(());
            }
            let Some(declaration) = declarations.get(name.as_str()) else {
                return Err(unprojectable(
                    action,
                    name,
                    "type declaration is unavailable",
                ));
            };
            match &declaration.body {
                TypeBody::Record { fields } => {
                    for field in fields {
                        visit_type(&field.ty, action, declarations, reached)?;
                    }
                    Ok(())
                }
                TypeBody::Enum { .. } => Ok(()),
                TypeBody::SchemaInline { .. } | TypeBody::SchemaImport { .. } => {
                    Err(unprojectable(
                        action,
                        name,
                        "JSON Schema-backed types have no lossless Rust projection",
                    ))
                }
            }
        }
    }
}

fn unprojectable(action: &ActionDecl, ty: &str, reason: &str) -> ScaffoldRefusal {
    ScaffoldRefusal::UnscaffoldableActionType {
        action: action.name.clone(),
        r#type: ty.to_owned(),
        reason: reason.to_owned(),
    }
}

fn emit_type(output: &mut String, declaration: &TypeDecl) {
    match &declaration.body {
        TypeBody::Record { fields } => {
            output.push_str("#[derive(Debug, Deserialize, Serialize)]\n");
            append(
                output,
                format_args!("struct {} {{\n", pascal(&declaration.name)),
            );
            for field in fields {
                append(
                    output,
                    format_args!(
                        "    {}: {},\n",
                        rust_ident(&field.name),
                        rust_type(&field.ty)
                    ),
                );
            }
            output.push_str("}\n\n");
        }
        TypeBody::Enum { variants } => {
            output.push_str("#[derive(Debug, Deserialize, Serialize)]\n");
            append(
                output,
                format_args!("enum {} {{\n", pascal(&declaration.name)),
            );
            for variant in variants {
                append(output, format_args!("    {},\n", pascal(&variant.name)));
            }
            output.push_str("}\n\n");
        }
        TypeBody::SchemaInline { .. } | TypeBody::SchemaImport { .. } => {}
    }
}

fn emit_action(output: &mut String, action: &ActionDecl) {
    let input = format!("{}Input", pascal(&action.name));
    output.push_str("#[derive(Debug, Deserialize, Serialize)]\n");
    append(output, format_args!("struct {input} {{\n"));
    for parameter in &action.params {
        append(
            output,
            format_args!(
                "    {}: {},\n",
                rust_ident(&parameter.name),
                rust_type(&parameter.ty)
            ),
        );
    }
    output.push_str("}\n\n");
    append(
        output,
        format_args!(
            "fn {}(_input: {input}, _context: &ActivityContext) -> HandlerFuture<'_, {}> {{\n    Box::pin(async move {{ todo!(\"implement {}\") }})\n}}\n\n",
            rust_ident(&action.name),
            rust_type(&action.returns),
            action.name
        ),
    );
}

fn append(output: &mut String, arguments: fmt::Arguments<'_>) {
    if output.write_fmt(arguments).is_err() {
        unreachable!("writing formatted text into a String cannot fail");
    }
}

fn rust_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Named { name, .. } => match name.as_str() {
            "String" => "String".to_owned(),
            "Int" => "i64".to_owned(),
            "Float" => "f64".to_owned(),
            "Bool" => "bool".to_owned(),
            "Nil" => "()".to_owned(),
            other => pascal(other),
        },
        TypeRef::List { inner, .. } => format!("Vec<{}>", rust_type(inner)),
        TypeRef::Optional { inner, .. } => format!("Option<{}>", rust_type(inner)),
    }
}

fn is_builtin(name: &str) -> bool {
    matches!(name, "String" | "Int" | "Float" | "Bool" | "Nil")
}

fn is_string(ty: &TypeRef) -> bool {
    matches!(ty, TypeRef::Named { name, .. } if name == "String")
}

fn pascal(name: &str) -> String {
    name.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect()
}

fn rust_ident(name: &str) -> String {
    if matches!(
        name,
        "as" | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
    ) {
        format!("r#{name}")
    } else {
        name.to_owned()
    }
}
