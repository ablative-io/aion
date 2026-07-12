use serde::{Deserialize, Serialize};

use super::handlers::Diagnostic;

#[derive(Debug, Deserialize)]
pub struct EditRequest {
    pub source: String,
    pub operation: EditOperation,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EditOperation {
    AddStep {
        name: String,
        #[serde(default)]
        prose: String,
    },
    AddAction {
        worker: String,
        name: String,
        params: Vec<ActionParameter>,
        return_type: String,
    },
    AddOutcomeRoute {
        source: String,
        target: String,
        name: String,
        guard: RouteGuard,
        #[serde(default)]
        payload: Vec<RouteArgument>,
    },
    AddFallThrough {
        source: String,
        target: String,
    },
    EditProse {
        step: String,
        prose: String,
    },
    RenameBinding {
        kind: RenameKind,
        from: String,
        to: String,
    },
    DeleteStep {
        step: String,
    },
}

#[derive(Debug, Deserialize)]
pub struct ActionParameter {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Deserialize)]
pub struct RouteArgument {
    pub name: String,
    pub expression: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RouteGuard {
    When { expression: String },
    Otherwise,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RenameKind {
    Step,
    Binding,
}

#[derive(Debug, Serialize)]
pub struct EditResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<EditRefusal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rename: Option<RenameMapping>,
}

#[derive(Debug, Serialize)]
pub struct RenameMapping {
    pub kind: RenameKind,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Serialize)]
pub struct EditRefusal {
    pub code: RefusalCode,
    pub message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    InvalidSource,
    InvalidOperation,
    UnknownStep,
    UnknownWorker,
    UnknownBinding,
    NameCollision,
    InvalidRouteTarget,
    StepInUse,
    FallThroughUnavailable,
    CanonicalizationFailed,
}

pub(super) type EditResult<T> = Result<T, EditRefusal>;
