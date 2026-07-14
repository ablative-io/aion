use std::collections::{BTreeMap, BTreeSet};

use super::{
    DEFAULT_ADDRESS, PostRunCommit, Profiles, Role, SHELL_NODE, Shell, inner_norn_harness,
    parse_args_from, prompts, roles, shell_registry,
};

fn parse(args: &[&str]) -> anyhow::Result<super::Args> {
    parse_args_from(
        args.iter().map(|arg| (*arg).to_owned()),
        "norn-default".to_owned(),
    )
}

fn profiles() -> Profiles {
    Profiles {
        developer: "dev".to_owned(),
        reviewer: "rev".to_owned(),
    }
}

#[test]
fn profiles_dir_is_required() -> anyhow::Result<()> {
    let Err(error) = parse(&[]) else {
        anyhow::bail!("arguments unexpectedly parsed without --profiles-dir");
    };
    assert!(
        error.to_string().contains("--profiles-dir"),
        "error: {error}"
    );
    Ok(())
}

#[test]
fn minimal_arguments_yield_the_defaults() -> anyhow::Result<()> {
    let args = parse(&["--profiles-dir", "/pkg/profiles"])?;
    assert_eq!(args.candidates, vec![DEFAULT_ADDRESS.to_owned()]);
    assert_eq!(args.identity_prefix, "dev-brief-worker");
    assert_eq!(args.ready_file, None);
    assert_eq!(args.norn_bin, "norn-default");
    assert_eq!(args.profiles_dir, "/pkg/profiles");
    Ok(())
}

#[test]
fn every_value_taking_flag_bails_when_missing() {
    for flag in [
        "--address",
        "--identity",
        "--ready-file",
        "--norn-bin",
        "--profiles-dir",
    ] {
        assert_eq!(
            parse(&[flag]).err().map(|error| error.to_string()),
            Some(format!("{flag} requires a value")),
        );
    }
}

#[test]
fn unknown_argument_bails() {
    assert_eq!(
        parse(&["--bogus"]).err().map(|error| error.to_string()),
        Some("unknown argument `--bogus`".to_owned()),
    );
}

/// The role wiring: each role carries its own schema, session suffix,
/// profile, and tool deny-list. The workspace root is NO LONGER a role
/// field — it is per-run data the harness reads from the activity input —
/// so the deny-list is the discriminator the reviewer carries and the
/// developer does not.
#[test]
fn roles_bind_schema_session_profile_and_deny_list() {
    let roles = roles(profiles());
    let summary: Vec<(&str, &str, Option<&str>)> = roles
        .iter()
        .map(|role| {
            (
                role.activity_type,
                role.session_suffix,
                role.disallowed_tools,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("developer", "developer", None),
            ("review_lens", "reviewer", Some("write,edit,apply_patch")),
        ]
    );
    for role in &roles {
        assert!(role.output_schema.trim_start().starts_with('{'));
        assert!(!role.profile.is_empty());
    }
}

/// The reviewer's read-only guarantee at the process boundary: its
/// composed Norn command carries `--disallowed-tools` naming exactly the
/// file-mutating tools (`write`, `edit`, `apply_patch`), and the developer
/// carries no deny-list at all (it must write to implement the brief).
#[test]
fn the_reviewer_denies_file_mutating_tools_and_the_developer_does_not() {
    let roles = roles(profiles());
    for role in &roles {
        let debug = format!("{:?}", inner_norn_harness("norn", role));
        if role.activity_type == "review_lens" {
            assert!(
                debug.contains("\"--disallowed-tools\", \"write,edit,apply_patch\""),
                "the reviewer must deny the file-mutating tools; args were:\n{debug}"
            );
        } else {
            assert_eq!(role.activity_type, "developer");
            assert!(
                !debug.contains("--disallowed-tools"),
                "the developer must carry no tool deny-list; args were:\n{debug}"
            );
        }
        // No role bakes a static --workspace-root: it is per-run input data.
        assert!(
            !debug.contains("--workspace-root"),
            "the workspace root is per-run input, never a static harness arg; \
                 args were:\n{debug}"
        );
    }
}

/// The mechanical-git doctrine's wiring, the FULL table: the developer
/// commits its round's work (the report's `commits` is rewritten to the
/// real head), and no other role's harness may grow a silent git side
/// effect.
#[test]
fn post_run_commits_are_wired_per_role_exactly() {
    let table: Vec<(&str, Option<PostRunCommit>)> = roles(profiles())
        .iter()
        .map(|role| (role.activity_type, role.post_run_commit))
        .collect();
    assert_eq!(
        table,
        vec![
            ("developer", Some(PostRunCommit::DevWork)),
            ("review_lens", None),
        ]
    );
}

/// The routing contract this worker's three connections uphold: the
/// server routes by (namespace, `task_queue`, node) only, so the node
/// table must be INJECTIVE (no two connections share a node) and
/// EXHAUSTIVE (every served activity type maps to exactly one node).
/// Reads the served sets from the production `roles`/`shell_registry`
/// definitions so the guard cannot drift from what actually registers.
#[test]
fn node_mapping_is_exhaustive_and_injective() -> anyhow::Result<()> {
    let roles = roles(profiles());

    let mut nodes: BTreeSet<&str> = roles.iter().map(|role| role.node).collect();
    assert_eq!(nodes.len(), roles.len(), "two agent roles share a node");
    assert!(
        nodes.insert(SHELL_NODE),
        "an agent role reuses the shell connection's node"
    );

    let registry = shell_registry(Shell::inherited())?;
    let mut activity_to_node: BTreeMap<String, &str> = BTreeMap::new();
    for activity_type in registry.activity_types() {
        assert!(
            activity_to_node
                .insert(activity_type.clone(), SHELL_NODE)
                .is_none(),
            "shell activity `{activity_type}` registered twice"
        );
    }
    for role in &roles {
        assert!(
            activity_to_node
                .insert(role.activity_type.to_owned(), role.node)
                .is_none(),
            "activity `{}` is served on two nodes",
            role.activity_type
        );
    }
    assert_eq!(
        activity_to_node
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![
            "cleanup_workspace",
            "developer",
            "format_verdict_evidence",
            "provision_workspace",
            "reset_workspace",
            "review_lens",
            "run_gates",
            "verify_gates",
        ],
        "the served activity-type set changed; keep both transition drivers' \
             shell-node contracts synchronized"
    );
    Ok(())
}

/// Pin the exact node-id strings to the workflow-side source of truth
/// (`shell_node`/`developer_node`/`reviewer_node` in
/// `src/dev_brief/activities.gleam`). The server matches these strings
/// blindly; a drift on either side strands activities on handlerless
/// connections.
#[test]
fn node_ids_mirror_the_workflow_constants() {
    assert_eq!(SHELL_NODE, "shell");
    let nodes: Vec<&str> = roles(profiles()).iter().map(|role| role.node).collect();
    assert_eq!(nodes, vec!["developer", "reviewer"]);
}

/// The role's profile doctrine reaches Norn as `--append-system-prompt`
/// (which APPENDS to Norn's own system prompt — never `--system-prompt`,
/// which would OVERWRITE it), and the profile text follows that flag
/// byte-identical. The "profile byte-identical in the prompt" contract
/// moved here from the per-turn prompt assembly.
#[test]
fn the_profile_rides_as_append_system_prompt_byte_identical() {
    let role = Role {
        activity_type: "developer",
        node: "developer",
        output_schema: "{}",
        session_suffix: "developer",
        profile: "MARKER_PROFILE_TEXT".to_owned(),
        assemble: prompts::developer,
        disallowed_tools: None,
        post_run_commit: Some(PostRunCommit::DevWork),
    };
    let debug = format!("{:?}", inner_norn_harness("norn", &role));
    assert!(
        debug.contains("\"--append-system-prompt\", \"MARKER_PROFILE_TEXT\""),
        "the profile must ride as the value immediately after \
             --append-system-prompt, byte-identical; args were:\n{debug}"
    );
    assert!(
        !debug.contains("\"--system-prompt\""),
        "the doctrine must APPEND, never OVERWRITE Norn's system prompt"
    );
}
