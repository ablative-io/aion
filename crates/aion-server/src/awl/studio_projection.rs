use aion_awl::{Document, TypeBody, TypeRef};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct StudioProjection {
    pub builtins: &'static [&'static str],
    pub types: Vec<StudioType>,
    pub workers: Vec<StudioWorker>,
}

#[derive(Debug, Serialize)]
pub struct StudioType {
    pub name: String,
    pub kind: StudioTypeKind,
    pub fields: Vec<StudioField>,
    pub variants: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StudioTypeKind {
    Record,
    Enum,
    Schema,
}

#[derive(Debug, Serialize)]
pub struct StudioField {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Serialize)]
pub struct StudioWorker {
    pub name: String,
    pub actions: Vec<StudioAction>,
}

#[derive(Debug, Serialize)]
pub struct StudioAction {
    pub name: String,
    pub params: Vec<StudioField>,
    pub return_type: String,
}

pub(super) fn build(document: &Document) -> StudioProjection {
    StudioProjection {
        builtins: &["Bool", "Int", "Float", "String", "Nil", "Dir"],
        types: document
            .types
            .iter()
            .map(|declaration| {
                let (kind, fields, variants) = match &declaration.body {
                    TypeBody::Record { fields } => (
                        StudioTypeKind::Record,
                        fields
                            .iter()
                            .map(|field| StudioField {
                                name: field.name.clone(),
                                ty: render_type(&field.ty),
                            })
                            .collect(),
                        Vec::new(),
                    ),
                    TypeBody::Enum { variants } => (
                        StudioTypeKind::Enum,
                        Vec::new(),
                        variants
                            .iter()
                            .map(|variant| variant.name.clone())
                            .collect(),
                    ),
                    TypeBody::SchemaInline { .. } | TypeBody::SchemaImport { .. } => {
                        (StudioTypeKind::Schema, Vec::new(), Vec::new())
                    }
                };
                StudioType {
                    name: declaration.name.clone(),
                    kind,
                    fields,
                    variants,
                }
            })
            .collect(),
        workers: document
            .workers
            .iter()
            .map(|worker| StudioWorker {
                name: worker.name.clone(),
                actions: worker
                    .actions
                    .iter()
                    .map(|action| StudioAction {
                        name: action.name.clone(),
                        params: action
                            .params
                            .iter()
                            .map(|parameter| StudioField {
                                name: parameter.name.clone(),
                                ty: render_type(&parameter.ty),
                            })
                            .collect(),
                        return_type: render_type(&action.returns),
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn render_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Named { name, .. } => name.clone(),
        TypeRef::List { inner, .. } => format!("[{}]", render_type(inner)),
        TypeRef::Optional { inner, .. } => format!("{}?", render_type(inner)),
    }
}
