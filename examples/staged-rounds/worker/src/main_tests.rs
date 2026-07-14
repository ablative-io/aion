//! Composition-root tests: argument parsing (incl. `--max-parallel`), the
//! role table (nodes, schemas, deny-lists, concurrency), the per-run
//! session mechanic's wiring, and the node-routing contract.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    COORDINATOR_CONCURRENCY, DEFAULT_ADDRESS, Profiles, Role, SHELL_NODE, Shell,
    inner_norn_harness, parse_args_from, roles, shell_registry,
};

fn parse(args: &[&str]) -> anyhow::Result<super::Args> {
    parse_args_from(
        args.iter().map(|arg| (*arg).to_owned()),
        "norn-default".to_owned(),
    )
}

fn profiles() -> Profiles {
    Profiles {
        planner: "plan".to_owned(),
        developer: "dev".to_owned(),
        reviewer: "rev".to_owned(),
        remediator: "rem".to_owned(),
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
    assert_eq!(args.identity_prefix, "staged-rounds-worker");
    assert_eq!(args.ready_file, None);
    assert_eq!(args.norn_bin, "norn-default");
    assert_eq!(args.profiles_dir, "/pkg/profiles");
    assert_eq!(args.max_parallel, 4);
    Ok(())
}

#[test]
fn max_parallel_parses_and_rejects_zero() -> anyhow::Result<()> {
    let args = parse(&["--profiles-dir", "/p", "--max-parallel", "7"])?;
    assert_eq!(args.max_parallel, 7);
    let Err(error) = parse(&["--profiles-dir", "/p", "--max-parallel", "0"]) else {
        anyhow::bail!("--max-parallel 0 unexpectedly parsed");
    };
    assert!(error.to_string().contains("at least 1"), "error: {error}");
    assert!(parse(&["--profiles-dir", "/p", "--max-parallel", "x"]).is_err());
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
        "--max-parallel",
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

/// The role wiring: activity type, node, deny-list, and concurrency per
/// role. The developer and reviewer take the operator's `--max-parallel`
/// (the genuine-parallelism knob); the coordinator roles stay narrow.
#[test]
fn roles_bind_node_deny_list_and_concurrency() {
    let roles = roles(profiles(), 6);
    let summary: Vec<(&str, &str, Option<&str>, usize)> = roles
        .iter()
        .map(|role| {
            (
                role.activity_type,
                role.node,
                role.disallowed_tools,
                role.max_concurrency,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "planner",
                "planner",
                Some("write,edit,apply_patch"),
                COORDINATOR_CONCURRENCY
            ),
            ("dev_item", "developer", None, 6),
            ("review_item", "reviewer", Some("write,edit,apply_patch"), 6),
            ("remediate", "remediation", None, COORDINATOR_CONCURRENCY),
        ]
    );
    for role in &roles {
        assert!(role.output_schema.trim_start().starts_with('{'));
        assert!(!role.profile.is_empty());
    }
}

/// The per-run session mechanic's wiring: NO role bakes a static
/// `--session-id` or `--workspace-root` into its composed Norn command —
/// both are per-run values the wrapper appends at start (the arg template
/// cannot express a per-item session id). The read-only roles carry the
/// file-mutating deny-list; the writing roles carry none.
#[test]
fn no_static_session_or_workspace_args_and_deny_lists_split_by_role() {
    for role in roles(profiles(), 4) {
        let debug = format!("{:?}", inner_norn_harness("norn", &role));
        assert!(
            !debug.contains("--session-id"),
            "role {} must not bake a static --session-id; args were:\n{debug}",
            role.activity_type
        );
        assert!(
            !debug.contains("--resume-if-exists"),
            "role {} must not bake --resume-if-exists (the wrapper appends it \
             with the derived id); args were:\n{debug}",
            role.activity_type
        );
        assert!(
            !debug.contains("--workspace-root"),
            "role {} must not bake a static --workspace-root; args were:\n{debug}",
            role.activity_type
        );
        let read_only = matches!(role.activity_type, "planner" | "review_item");
        assert_eq!(
            debug.contains("\"--disallowed-tools\", \"write,edit,apply_patch\""),
            read_only,
            "role {} deny-list wiring; args were:\n{debug}",
            role.activity_type
        );
    }
}

/// The derived session suffixes, read through each role's production
/// extractor: per-item for the fan-out roles, and the SAME `planner` suffix
/// for the planner and remediator — the resumed-coordinator mechanic.
#[test]
fn session_suffixes_derive_per_role() -> anyhow::Result<()> {
    let roles = roles(profiles(), 4);
    let inputs: BTreeMap<&str, String> = BTreeMap::from([
        (
            "planner",
            "{\"material\":{},\"repo_root\":\"/r\",\"workspace_path\":\"/r\"}".to_owned(),
        ),
        (
            "dev_item",
            "{\"work\":{\"item\":{\"id\":\"it-9\",\"title\":\"t\",\"goal\":\"g\",\"phase\":1,\
             \"feedback\":\"\"},\"workspace_path\":\"/w\",\"branch\":\"b\",\
             \"base_commit\":\"c\"},\"gates\":[]}"
                .to_owned(),
        ),
        (
            "review_item",
            "{\"work\":{\"item\":{\"id\":\"it-9\",\"title\":\"t\",\"goal\":\"g\",\"phase\":1,\
             \"feedback\":\"\"},\"workspace_path\":\"/w\",\"branch\":\"b\",\
             \"base_commit\":\"c\",\"report\":{\"item_id\":\"it-9\",\"summary\":\"s\"}}}"
                .to_owned(),
        ),
        (
            "remediate",
            "{\"merge\":{},\"plan\":{},\"workspace_path\":\"/w/integration\"}".to_owned(),
        ),
    ]);
    let mut suffixes: BTreeMap<&str, String> = BTreeMap::new();
    for role in &roles {
        let input = inputs
            .get(role.activity_type)
            .ok_or_else(|| anyhow::anyhow!("no test input for {}", role.activity_type))?;
        let context = (role.extract)(input).map_err(anyhow::Error::msg)?;
        suffixes.insert(role.activity_type, context.session_suffix);
    }
    assert_eq!(suffixes["planner"], "planner");
    assert_eq!(suffixes["dev_item"], "dev-it-9");
    assert_eq!(suffixes["review_item"], "review-it-9");
    assert_eq!(
        suffixes["remediate"], "planner",
        "the remediator must resume the planner's session"
    );
    Ok(())
}

/// The routing contract this worker's five connections uphold: the server
/// routes by (namespace, `task_queue`, node) only, so the node table must
/// be INJECTIVE (no two connections share a node) and EXHAUSTIVE (every
/// served activity type maps to exactly one node). Reads the served sets
/// from the production `roles`/`shell_registry` definitions so the guard
/// cannot drift from what actually registers.
#[test]
fn node_mapping_is_exhaustive_and_injective() -> anyhow::Result<()> {
    let roles = roles(profiles(), 4);

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
            "dev_item",
            "fold_phase",
            "merge_branches",
            "planner",
            "provision_item",
            "remediate",
            "review_item",
        ],
        "the served activity-type set changed; keep the worker and the AWL \
         document's action table synchronized"
    );
    Ok(())
}

/// Pin the exact node-id strings to the document-side source of truth (the
/// `node` lines in `../awl/staged_rounds.awl`, re-verified independently by
/// `tests/compile_direct.rs`). The server matches these strings blindly; a
/// drift on either side strands activities on handlerless connections.
#[test]
fn node_ids_mirror_the_document_table() {
    assert_eq!(SHELL_NODE, "shell");
    let nodes: Vec<&str> = roles(profiles(), 4).iter().map(|role| role.node).collect();
    assert_eq!(
        nodes,
        vec!["planner", "developer", "reviewer", "remediation"]
    );
}

/// The role's profile doctrine reaches Norn as `--append-system-prompt`
/// (which APPENDS to Norn's own system prompt — never `--system-prompt`,
/// which would OVERWRITE it), and the profile text follows that flag
/// byte-identical.
#[test]
fn the_profile_rides_as_append_system_prompt_byte_identical() {
    let role = Role {
        activity_type: "dev_item",
        node: "developer",
        output_schema: "{}",
        profile: "MARKER_PROFILE_TEXT".to_owned(),
        assemble: super::prompts::dev_item,
        extract: super::harness::dev_item_context,
        disallowed_tools: None,
        max_concurrency: 4,
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
