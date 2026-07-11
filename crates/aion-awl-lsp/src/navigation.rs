use std::path::Path;

use aion_awl::{Document, Span, Statement, Step};
use lsp_types::{
    Hover, HoverContents, Location, MarkupContent, MarkupKind, Position, Range, SymbolKind, Uri,
};
use serde::Serialize;

use crate::analysis::{byte_offset_at, range_for_span};

#[derive(Serialize)]
struct OutlineSymbol {
    name: String,
    kind: SymbolKind,
    range: Range,
    #[serde(rename = "selectionRange")]
    selection_range: Range,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<OutlineSymbol>,
}

impl OutlineSymbol {
    fn new(
        source: &str,
        name: &str,
        kind: SymbolKind,
        span: Span,
        name_span: Span,
        children: Vec<Self>,
    ) -> Self {
        Self {
            name: name.to_owned(),
            kind,
            range: range_for_span(source, span),
            selection_range: range_for_span(source, name_span),
            children,
        }
    }
}

pub(crate) fn document_symbols(source: &str) -> Option<serde_json::Value> {
    let document = aion_awl::parse(source).ok()?;
    let mut workflow_children = Vec::new();
    workflow_children.extend(document.types.iter().map(|declaration| {
        OutlineSymbol::new(
            source,
            &declaration.name,
            SymbolKind::STRUCT,
            declaration.span,
            declaration.name_span,
            Vec::new(),
        )
    }));
    workflow_children.extend(document.workers.iter().map(|worker| {
        let actions = worker
            .actions
            .iter()
            .map(|action| {
                OutlineSymbol::new(
                    source,
                    &action.name,
                    SymbolKind::FUNCTION,
                    action.span,
                    action.name_span,
                    Vec::new(),
                )
            })
            .collect();
        OutlineSymbol::new(
            source,
            &worker.name,
            SymbolKind::NAMESPACE,
            worker.span,
            worker.name_span,
            actions,
        )
    }));
    workflow_children.extend(document.steps.iter().map(|step| step_symbol(source, step)));
    let symbols = vec![OutlineSymbol::new(
        source,
        &document.name,
        SymbolKind::MODULE,
        document.span,
        document.name_span,
        workflow_children,
    )];
    serde_json::to_value(symbols).ok()
}

fn step_symbol(source: &str, step: &Step) -> OutlineSymbol {
    let children = step
        .body
        .iter()
        .filter_map(|statement| match statement {
            Statement::SubStep(substep) => Some(step_symbol(source, substep)),
            _ => None,
        })
        .collect();
    OutlineSymbol::new(
        source,
        &step.name,
        SymbolKind::METHOD,
        step.span,
        step.name_span,
        children,
    )
}

pub(crate) fn definition(
    source: &str,
    uri: &Uri,
    position: Position,
    root: Option<&Path>,
) -> Option<Location> {
    let document = aion_awl::parse(source).ok()?;
    let analysis = analyze(&document, root);
    let byte = byte_offset_at(source, position);
    let declaration = analysis.info_at(byte)?.declaration.as_ref()?;
    Some(Location::new(
        uri.clone(),
        range_for_span(source, declaration.span),
    ))
}

pub(crate) fn hover(source: &str, position: Position, root: Option<&Path>) -> Option<Hover> {
    let document = aion_awl::parse(source).ok()?;
    let analysis = analyze(&document, root);
    let byte = byte_offset_at(source, position);
    let info = analysis.info_at(byte)?;
    let declaration = info.declaration.as_ref();
    let mut value = String::new();
    if let Some(declaration) = declaration {
        value.push_str("```awl\n");
        value.push_str(declaration.kind.as_str());
        value.push(' ');
        value.push_str(&declaration.name);
        if let Some(ty) = &info.ty {
            value.push_str(": ");
            value.push_str(ty);
        }
        value.push_str("\n```");
        if let Some(documentation) = &declaration.documentation {
            value.push_str("\n\n");
            value.push_str(documentation);
        }
    } else if let Some(ty) = &info.ty {
        value.push_str("```awl\n");
        value.push_str(ty);
        value.push_str("\n```");
    } else {
        return None;
    }
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(range_for_span(source, info.span)),
    })
}

fn analyze(document: &Document, root: Option<&Path>) -> aion_awl::semantic::SemanticAnalysis {
    root.map_or_else(
        || aion_awl::semantic::analyze(document),
        |root| aion_awl::semantic::analyze_in(document, root),
    )
}
