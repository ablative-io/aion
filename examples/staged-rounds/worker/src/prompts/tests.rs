//! Prompt-assembly tests: each role's prompt carries the fenced context,
//! the dev prompt carries the verbatim gate argv, and the reviewer prompt
//! names the exact `git diff <base>` command.

use super::{dev_item, planner, remediate, review_item};

const DEV_CONTEXT: &str = r#"{
    "work": {
        "item": {
            "id": "it-1",
            "title": "t",
            "goal": "g",
            "scope_in": ["src/"],
            "scope_out": ["docs/"],
            "phase": 1,
            "depends_on": [],
            "feedback": ""
        },
        "workspace_path": "/repo/.staged-rounds/wf/items/it-1",
        "branch": "staged/wf/it-1",
        "base_commit": "abc123"
    },
    "gates": [
        {"name": "fmt", "argv": ["cargo", "fmt", "--all"]},
        {"name": "test", "argv": ["cargo", "test", "--workspace"]}
    ]
}"#;

#[test]
fn every_role_prompt_carries_the_fenced_context() {
    for assemble in [planner, dev_item, review_item, remediate] {
        let prompt = assemble("{\"marker\":\"CONTEXT_MARKER\"}");
        assert!(prompt.contains("```json"), "missing fence:\n{prompt}");
        assert!(
            prompt.contains("CONTEXT_MARKER"),
            "missing context:\n{prompt}"
        );
    }
}

#[test]
fn the_dev_prompt_lists_the_exact_gate_argv() {
    let prompt = dev_item(DEV_CONTEXT);
    assert!(
        prompt.contains("`cargo fmt --all`"),
        "missing fmt argv:\n{prompt}"
    );
    assert!(
        prompt.contains("`cargo test --workspace`"),
        "missing test argv:\n{prompt}"
    );
    assert!(prompt.contains("GATE DISCIPLINE"), "{prompt}");
}

#[test]
fn an_empty_gate_battery_is_named_explicitly() {
    let prompt = dev_item("{\"work\":{},\"gates\":[]}");
    assert!(
        prompt.contains("configured gate battery is empty"),
        "{prompt}"
    );
}

#[test]
fn the_reviewer_prompt_names_the_exact_diff_command() {
    let prompt = review_item(DEV_CONTEXT);
    assert!(
        prompt.contains("git diff abc123"),
        "missing base-commit diff command:\n{prompt}"
    );
    assert!(prompt.contains("READ-ONLY"), "{prompt}");
}

#[test]
fn a_reviewer_context_missing_the_base_renders_the_placeholder() {
    let prompt = review_item("{}");
    assert!(prompt.contains("git diff <base_commit>"), "{prompt}");
}

#[test]
fn the_remediate_prompt_frames_the_resumed_planner() {
    let prompt = remediate("{\"merge\":{},\"plan\":{},\"workspace_path\":\"/x\"}");
    assert!(prompt.contains("planner, resumed"), "{prompt}");
    assert!(prompt.contains("IN"), "{prompt}");
    assert!(prompt.contains("Do NOT run `git commit`"), "{prompt}");
}

#[test]
fn the_planner_prompt_states_the_parallel_safety_rule() {
    let prompt = planner("{\"material\":{},\"repo_root\":\"/r\",\"workspace_path\":\"/r\"}");
    assert!(prompt.contains("IN PARALLEL"), "{prompt}");
    assert!(prompt.contains("git-ref-safe slug"), "{prompt}");
}
