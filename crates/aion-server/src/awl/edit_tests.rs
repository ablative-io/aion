use super::edit::{
    ActionDefinition, ActionParameter, EditOperation, EditRequest, EditResponse, RefusalCode,
    RenameKind, RouteArgument, RouteGuard, TypeField, edit_source,
};
use super::handlers::{CheckRequest, check_source};

const FALL_THROUGH: &str =
    include_str!("../../../aion-awl/tests/fixtures/rev2/dag-fork/valid/fall_through_chain.awl");
const AFTER: &str =
    include_str!("../../../aion-awl/tests/fixtures/rev2/dag-fork/valid/after_single.awl");
const FIVE_STEP: &str = include_str!("../../tests/fixtures/p2_five_step.awl");
const EMPTY: &str = "//! Canvas gestures build this workflow.\nworkflow canvas_five\n  outcome done: type Result, route success\n\ntype Result { value: String }\n";
const STUDIO: &str = "//! Studio edits.\nworkflow studio\n  outcome done: type String, route success\n\ntype Item { value: String, spare: String }\n\nworker jobs\n  action used() -> String\n  action spare() -> String\n";

#[test]
fn every_v1_gesture_returns_byte_canonical_source() -> Result<(), String> {
    let cases = [
        apply(
            FALL_THROUGH,
            EditOperation::AddStep {
                name: "archive".to_owned(),
                prose: "Archive the published page.".to_owned(),
            },
        ),
        apply(
            FALL_THROUGH,
            EditOperation::AddAction {
                worker: "pipeline".to_owned(),
                name: "archive".to_owned(),
                params: vec![ActionParameter {
                    name: "url".to_owned(),
                    ty: "String".to_owned(),
                }],
                return_type: "Bool".to_owned(),
            },
        ),
        apply(
            FALL_THROUGH,
            EditOperation::AddOutcomeRoute {
                source: "clean_input".to_owned(),
                target: "render_page".to_owned(),
                name: "ready".to_owned(),
                guard: RouteGuard::When {
                    expression: "cleaned.text == \"ready\"".to_owned(),
                },
                payload: Vec::new(),
            },
        ),
        apply(
            FALL_THROUGH,
            EditOperation::AddOutcomeRoute {
                source: "clean_input".to_owned(),
                target: "render_page".to_owned(),
                name: "remaining".to_owned(),
                guard: RouteGuard::Otherwise,
                payload: Vec::new(),
            },
        ),
        apply(
            FALL_THROUGH,
            EditOperation::AddFallThrough {
                source: "clean_input".to_owned(),
                target: "render_page".to_owned(),
            },
        ),
        apply(
            FALL_THROUGH,
            EditOperation::EditProse {
                step: "render_page".to_owned(),
                prose: "Render the clean input.\nKeep the canonical page.".to_owned(),
            },
        ),
        apply(
            AFTER,
            EditOperation::RenameBinding {
                kind: RenameKind::Step,
                from: "fetch_doc".to_owned(),
                to: "load_doc".to_owned(),
            },
        ),
        apply(
            FALL_THROUGH,
            EditOperation::RenameBinding {
                kind: RenameKind::Binding,
                from: "cleaned".to_owned(),
                to: "normalized".to_owned(),
            },
        ),
        apply(
            AFTER,
            EditOperation::DeleteStep {
                step: "summarize_doc".to_owned(),
            },
        ),
    ];

    for response in cases {
        let source = success_source(response)?;
        assert_eq!(aion_awl::print(&parse_source(&source)?), source);
    }
    Ok(())
}

#[test]
fn rename_reports_mapping_and_renames_all_semantic_references() -> Result<(), String> {
    let response = apply(
        FALL_THROUGH,
        EditOperation::RenameBinding {
            kind: RenameKind::Binding,
            from: "cleaned".to_owned(),
            to: "normalized".to_owned(),
        },
    );
    let mapping = response
        .rename
        .as_ref()
        .ok_or_else(|| "missing rename mapping".to_owned())?;
    assert!(matches!(mapping.kind, RenameKind::Binding));
    assert_eq!(mapping.from, "cleaned");
    assert_eq!(mapping.to, "normalized");
    let source = success_source(response)?;
    assert!(!source.contains("cleaned"));
    assert!(source.contains("-> normalized"));
    assert!(source.contains("normalized |>"));
    Ok(())
}

#[test]
fn typed_refusals_cover_collision_unknown_step_and_invalid_route_target() -> Result<(), String> {
    let collision = apply(
        AFTER,
        EditOperation::RenameBinding {
            kind: RenameKind::Step,
            from: "fetch_doc".to_owned(),
            to: "summarize_doc".to_owned(),
        },
    );
    assert_refusal(collision, &RefusalCode::NameCollision)?;

    let unknown = apply(
        AFTER,
        EditOperation::EditProse {
            step: "missing".to_owned(),
            prose: "Missing".to_owned(),
        },
    );
    assert_refusal(unknown, &RefusalCode::UnknownStep)?;

    let invalid_target = apply(
        AFTER,
        EditOperation::AddOutcomeRoute {
            source: "fetch_doc".to_owned(),
            target: "missing".to_owned(),
            name: "missing".to_owned(),
            guard: RouteGuard::Otherwise,
            payload: Vec::new(),
        },
    );
    assert_refusal(invalid_target, &RefusalCode::InvalidRouteTarget)?;
    Ok(())
}

#[test]
fn p2_exit_bar_builds_five_step_workflow_byte_for_byte_and_checks_green() -> Result<(), String> {
    let mut source = EMPTY.to_owned();
    for (name, prose) in [
        ("receive", "Receive the request."),
        ("validate", "Validate the request."),
        ("process", "Process the request."),
        ("record", "Record the result."),
        ("finish", "Finish the workflow."),
    ] {
        source = success_source(apply(
            &source,
            EditOperation::AddStep {
                name: name.to_owned(),
                prose: prose.to_owned(),
            },
        ))?;
    }
    source = success_source(apply(
        &source,
        EditOperation::AddFallThrough {
            source: "receive".to_owned(),
            target: "validate".to_owned(),
        },
    ))?;
    source = success_source(apply(
        &source,
        EditOperation::AddOutcomeRoute {
            source: "finish".to_owned(),
            target: "done".to_owned(),
            name: "complete".to_owned(),
            guard: RouteGuard::Otherwise,
            payload: vec![RouteArgument {
                name: "value".to_owned(),
                expression: "\"complete\"".to_owned(),
            }],
        },
    ))?;

    assert_eq!(source, FIVE_STEP);
    assert_eq!(aion_awl::print(&parse_source(&source)?), source);
    let checked = check_source(&CheckRequest { source, path: None });
    assert!(checked.ok, "diagnostics: {:?}", checked.diagnostics);
    assert!(
        checked.diagnostics.is_empty(),
        "diagnostics: {:?}",
        checked.diagnostics
    );
    Ok(())
}

#[test]
fn every_studio_operation_returns_byte_canonical_source() -> Result<(), String> {
    let cases = [
        apply(
            STUDIO,
            EditOperation::AddType {
                name: "Order".to_owned(),
                fields: vec![TypeField {
                    name: "id".to_owned(),
                    ty: "String".to_owned(),
                }],
            },
        ),
        apply(
            STUDIO,
            EditOperation::AddTypeField {
                type_name: "Item".to_owned(),
                name: "count".to_owned(),
                field_type: "Int".to_owned(),
            },
        ),
        apply(
            STUDIO,
            EditOperation::RemoveTypeField {
                type_name: "Item".to_owned(),
                name: "spare".to_owned(),
            },
        ),
        apply(
            STUDIO,
            EditOperation::AddEnumType {
                name: "Status".to_owned(),
                variants: vec!["Pending".to_owned(), "Complete".to_owned()],
            },
        ),
        apply(
            STUDIO,
            EditOperation::AddWorker {
                name: "billing".to_owned(),
                action: ActionDefinition {
                    name: "charge".to_owned(),
                    params: vec![ActionParameter {
                        name: "item".to_owned(),
                        ty: "Item".to_owned(),
                    }],
                    return_type: "Bool".to_owned(),
                },
            },
        ),
        apply(
            STUDIO,
            EditOperation::RemoveWorker {
                name: "jobs".to_owned(),
            },
        ),
        apply(
            STUDIO,
            EditOperation::RemoveAction {
                worker: "jobs".to_owned(),
                name: "spare".to_owned(),
            },
        ),
    ];
    for response in cases {
        let source = success_source(response)?;
        assert_eq!(aion_awl::print(&parse_source(&source)?), source);
    }
    Ok(())
}

#[test]
fn studio_operations_return_specific_typed_refusals() -> Result<(), String> {
    assert_refusal(
        apply(
            STUDIO,
            EditOperation::AddType {
                name: "Item".to_owned(),
                fields: Vec::new(),
            },
        ),
        &RefusalCode::NameCollision,
    )?;
    assert_refusal(
        apply(
            STUDIO,
            EditOperation::AddTypeField {
                type_name: "Item".to_owned(),
                name: "missing".to_owned(),
                field_type: "Missing".to_owned(),
            },
        ),
        &RefusalCode::UnknownType,
    )?;
    assert_refusal(
        apply(
            STUDIO,
            EditOperation::RemoveAction {
                worker: "jobs".to_owned(),
                name: "missing".to_owned(),
            },
        ),
        &RefusalCode::UnknownAction,
    )?;
    let last_action = "//! Last action.\nworkflow last\n  outcome done: type String, route success\n\nworker jobs\n  action only() -> String\n";
    assert_refusal(
        apply(
            last_action,
            EditOperation::RemoveAction {
                worker: "jobs".to_owned(),
                name: "only".to_owned(),
            },
        ),
        &RefusalCode::LastAction,
    )?;
    let referenced = "//! References.\nworkflow references\n  outcome done: type String, route success\n\nworker jobs\n  action used() -> String\n  action spare() -> String\n\nstep finish\n  used() -> value\n  route done(value: value)\n";
    assert_refusal(
        apply(
            referenced,
            EditOperation::RemoveAction {
                worker: "jobs".to_owned(),
                name: "used".to_owned(),
            },
        ),
        &RefusalCode::ActionInUse,
    )?;
    assert_refusal(
        apply(
            referenced,
            EditOperation::RemoveWorker {
                name: "jobs".to_owned(),
            },
        ),
        &RefusalCode::ActionInUse,
    )?;
    let field_reference = "//! Field reference.\nworkflow fields\n  input item: Item\n  outcome done: type String, route success\n\ntype Item { value: String }\n\nstep finish\n  route done(value: item.value)\n";
    assert_refusal(
        apply(
            field_reference,
            EditOperation::RemoveTypeField {
                type_name: "Item".to_owned(),
                name: "value".to_owned(),
            },
        ),
        &RefusalCode::TypeInUse,
    )?;
    Ok(())
}

fn apply(source: &str, operation: EditOperation) -> EditResponse {
    edit_source(&EditRequest {
        source: source.to_owned(),
        operation,
    })
}

fn success_source(response: EditResponse) -> Result<String, String> {
    if !response.ok {
        return Err(format!("gesture refusal: {:?}", response.refusal));
    }
    response
        .source
        .ok_or_else(|| "successful response had no source".to_owned())
}

fn assert_refusal(response: EditResponse, expected: &RefusalCode) -> Result<(), String> {
    if response.ok {
        return Err(format!(
            "gesture unexpectedly succeeded; expected {expected:?}"
        ));
    }
    let refusal = response
        .refusal
        .ok_or_else(|| "refused response had no typed refusal".to_owned())?;
    assert_eq!(
        std::mem::discriminant(&refusal.code),
        std::mem::discriminant(expected)
    );
    Ok(())
}

fn parse_source(source: &str) -> Result<aion_awl::Document, String> {
    aion_awl::parse(source).map_err(|error| error.message)
}
