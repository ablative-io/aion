//! CLI, routing, registry, reconnect, and capability composition tests.

use std::collections::BTreeSet;
use std::error::Error;

use aion_integrations::InterventionPrimitive;
use general_worker::args::DEFAULT_ADDRESS;
use general_worker::composition::{PARSE_OUTPUT, agent_capabilities};
use general_worker::{
    ACTIVITY_NODE_MAP, AGENT_NODE, RUN_AGENT, RUN_COMMAND, SHELL_NODE, Shell, TASK_QUEUE,
    agent_config, agent_registry, build_worker_config, parse_args_from, shell_registry,
};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn cli_defaults_are_explicit_and_norn_uses_the_injected_environment_default() -> TestResult {
    let args = parse_args_from(Vec::<String>::new(), "env-norn".to_owned())?;
    assert_eq!(args.addresses, vec![DEFAULT_ADDRESS]);
    assert_eq!(args.identity, "general-worker");
    assert_eq!(args.ready_file, None);
    assert_eq!(args.norn_bin, "env-norn");
    assert_eq!(args.agent_activities, Vec::<String>::new());
    Ok(())
}

#[test]
fn cli_accepts_repeatable_addresses_and_all_overrides() -> TestResult {
    let args = parse_args_from(
        [
            "--address",
            "server-a:50061",
            "--address",
            "server-b:50061",
            "--identity",
            "worker-seven",
            "--ready-file",
            "/var/run/general.ready",
            "--norn-bin",
            "/opt/bin/norn",
            "--agent-activity",
            "select_wave",
            "--agent-activity",
            "land_wave",
        ]
        .into_iter()
        .map(str::to_owned),
        "ignored-default".to_owned(),
    )?;
    assert_eq!(args.addresses, vec!["server-a:50061", "server-b:50061"]);
    assert_eq!(args.identity, "worker-seven");
    assert_eq!(
        args.ready_file,
        Some(std::path::PathBuf::from("/var/run/general.ready"))
    );
    assert_eq!(args.norn_bin, "/opt/bin/norn");
    assert_eq!(args.agent_activities, vec!["select_wave", "land_wave"]);
    Ok(())
}

#[test]
fn cli_rejects_unknown_flags_missing_values_and_blank_values() -> TestResult {
    let unknown = parse_args_from(["--bogus".to_owned()], "norn".to_owned())
        .err()
        .ok_or("unknown flag must fail")?;
    assert_eq!(unknown.to_string(), "unknown argument `--bogus`");

    let missing = parse_args_from(["--identity".to_owned()], "norn".to_owned())
        .err()
        .ok_or("missing identity must fail")?;
    assert_eq!(missing.to_string(), "--identity requires a value");

    let blank = parse_args_from(["--address".to_owned(), "  ".to_owned()], "norn".to_owned())
        .err()
        .ok_or("blank address must fail")?;
    assert_eq!(blank.to_string(), "--address requires a nonblank value");
    Ok(())
}

#[test]
fn activity_node_map_is_exact() -> TestResult {
    let agent = build_worker_config("test-agent", AGENT_NODE)?;
    let shell = build_worker_config("test-shell", SHELL_NODE)?;
    assert_eq!(
        ACTIVITY_NODE_MAP,
        [
            (RUN_AGENT, AGENT_NODE),
            (RUN_COMMAND, SHELL_NODE),
            (PARSE_OUTPUT, SHELL_NODE),
        ]
    );
    assert_eq!(agent.task_queue, TASK_QUEUE);
    assert_eq!(agent.node, AGENT_NODE);
    assert_eq!(agent.identity, "test-agent");
    assert_eq!(agent.reconnect.max_attempts, usize::MAX);
    assert_eq!(shell.task_queue, TASK_QUEUE);
    assert_eq!(shell.node, SHELL_NODE);
    assert_eq!(shell.identity, "test-shell");
    assert_eq!(shell.reconnect.max_attempts, usize::MAX);
    Ok(())
}

#[test]
fn registries_and_agent_activity_advertisement_are_exact() -> TestResult {
    let agent_registry = agent_registry();
    let shell_registry = shell_registry(Shell::inherited())?;
    let agent = agent_config("norn-test", &[]);

    assert!(agent_registry.is_empty());
    assert_eq!(
        shell_registry.activity_types(),
        BTreeSet::from([RUN_COMMAND.to_owned(), PARSE_OUTPUT.to_owned()])
    );
    assert_eq!(
        agent.agent_activity_types(),
        &BTreeSet::from([RUN_AGENT.to_owned()])
    );
    Ok(())
}

#[test]
fn agent_activity_aliases_extend_the_advertisement_and_keep_run_agent() -> TestResult {
    let aliases = ["select_wave".to_owned(), "land_wave".to_owned()];
    let agent = agent_config("norn-test", &aliases);

    let advertised = agent.agent_activity_types();
    advertised
        .get(RUN_AGENT)
        .ok_or("run_agent must stay advertised alongside aliases")?;
    assert_eq!(
        advertised,
        &BTreeSet::from([
            RUN_AGENT.to_owned(),
            "select_wave".to_owned(),
            "land_wave".to_owned(),
        ])
    );
    Ok(())
}

#[test]
fn agent_capabilities_are_exactly_inject_and_cancel() -> TestResult {
    let capabilities = agent_capabilities();
    assert_eq!(
        capabilities.supported,
        vec![
            InterventionPrimitive::InjectMessage,
            InterventionPrimitive::Cancel,
        ]
    );
    let config = build_worker_config("capability-test", AGENT_NODE)?;
    assert_eq!(config.task_queue, TASK_QUEUE);
    Ok(())
}
