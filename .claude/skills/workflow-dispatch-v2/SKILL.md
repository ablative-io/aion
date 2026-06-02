---
name: workflow-dispatch-v2
description: Dispatch Meridian v2 workflows (orchestrated-dev, review-and-land, etc.) against the Yggdrasil substrate via the unified `meridian` binary. Use when running or monitoring workflows in the v2 repo at /Users/tom/Developer/ablative/yggdrasil. Triggered by terms like dispatch workflow, run workflow, orchestrated-dev, review-and-land, meridian workflow run, worktree dispatch, v2 workflow.
---

# Meridian v2 Workflow Dispatch

Dispatch Meridian v2 workflows via the `meridian workflow` CLI against the Yggdrasil substrate. This skill covers everything you need to run, monitor, and recover from workflows.

## Identity

**Your session ID:** `${CLAUDE_SESSION_ID}` — this is who you are.

**The `--as` identity:** workflow dispatch requires a member with a reporting-tree link to the Yggdrasil workspace. Raw Claude session IDs without that link are rejected with 403. Unless told otherwise, dispatch `--as c9255b2a-5731-4d17-8124-e3bfa2224186` (Tom's member ID).

When your own session has been linked into the workspace ACL, prefer dispatching `--as ${CLAUDE_SESSION_ID}`. The end-of-workflow notification DM is hard-wired to land in both Waffles' and Tom's inboxes regardless of `--as`, so identity here only affects authorisation, not who sees completion.

## Workspace and Binary

- **v2 workspace ID:** `2d5fdd51-1f25-45a4-8f86-4d4c978d1355`
- **Unified binary:** `/Users/tom/Developer/ablative/yggdrasil/target/release/meridian`
- **Server config:** `~/.meridian/v2-config.toml`
- **Workflow source:** `.meridian/workflows/*.yaml` in the repo (scanned at server startup). `.yggdrasil/workflows/` is NOT scanned.

Restart the v2 server after a rebuild or config change:

```bash
nohup ./target/release/meridian serve start --foreground --config ~/.meridian/v2-config.toml \
  > /tmp/meridian-v2-$(date +%Y%m%d-%H%M).log 2>&1 </dev/null & disown
```

## CLI Reference

```
meridian workflow list                                   # Available workflow names
meridian workflow show <name> --workspace <id>           # Definition: inputs, steps
meridian workflow run <name> --workspace <id> ...        # Dispatch a workflow
meridian workflow status <execution-id>                  # Status + per-step details
meridian workflow history --workspace <id> --limit N     # Past executions
meridian workflow cancel <execution-id>                  # Cancel a running execution
```

All commands default to JSON output; add `--text` for human-readable. All accept `--as <ID>` for identity.

## Core Workflows

Authoritative list is `meridian workflow list`; this table is the common set at time of writing.

| Workflow | Required inputs | Purpose |
|----------|----------------|---------|
| `orchestrated-dev` | `brief` (optional: `run-name`) | Scout → Plan → Implement → checks → Review → Done → **Notify**. Default for greenfield brief work. Pass `--input run-name="brief NNN — short description"` so the auto-notification DM identifies the run. |
| `review-and-land` | `brief`, `findings`, `commit_prefix` | Apply reviewer findings to an existing worktree, run full checks in a loop, commit. Used to close out an orchestrated-dev run that produced review output needing action. |
| `stack-land` | see `show` | Land stacked branch work. |
| `notify` | (optional: `workflow`, `run-name`) | End-of-workflow notification helper. Dispatched automatically from `orchestrated-dev`'s terminal step; rarely run by hand. Sends a DM to Waffles + Tom with branch, build counts, and lint-suppression delta vs `origin/main`. |
| `mechanical-review` / `smell-and-criterion-review` | see `show` | Review-only passes; no code changes. |
| `code-review` | see `show` | Reviewer-facing review pass. |

Always run `meridian workflow show <name> --workspace <id>` before dispatching something unfamiliar.

## Dispatch Patterns

### Fresh worktree (new feature branch from `main`)

```bash
meridian workflow run orchestrated-dev \
  --workspace 2d5fdd51-1f25-45a4-8f86-4d4c978d1355 \
  --as c9255b2a-5731-4d17-8124-e3bfa2224186 \
  --worktree --base main \
  --input "brief=$(cat /abs/path/to/brief.md)" \
  --input "run-name=brief 215 — libcorpus loaders"
```

The `run-name` input flows through to the end-of-workflow notification DM so the message identifies which brief it relates to. Optional but recommended.

### Existing worktree (follow-up work on a branch)

```bash
meridian workflow run review-and-land \
  --workspace 2d5fdd51-1f25-45a4-8f86-4d4c978d1355 \
  --as c9255b2a-5731-4d17-8124-e3bfa2224186 \
  --worktree /abs/path/to/.yggdrasil-worktrees/workflow/orchestrated-dev/<id> \
  --input "brief=$(cat /abs/path/to/brief.md)" \
  --input "findings=$(cat /abs/path/to/findings.md)" \
  --input "commit_prefix=feat: brief 123 (short description)"
```

### Main checkout (no worktree, run directly in the repo)

Omit `--worktree` entirely. Use sparingly; worktree isolation is the default.

### Worktree rules

- `--worktree` with **no value**: provision a fresh branch + worktree under `.yggdrasil-worktrees/workflow/<workflow-name>/<id>/`, based on `--base` (default `main`).
- `--worktree <PATH>`: use an **existing** worktree at that absolute path — the CLI does not provision, start, or tear down.
- `--worktree ""` (empty string): **rejected**. Pass a bare flag, a real path, or omit entirely. There is no silent fallback.
- **Never use `--worksite`.** Worksites are the v1 concept with known bugs (cwd escape). v2 uses worktrees exclusively.

### Always pass inputs as `--input key=value`

One `--input` per named input in the workflow's schema. Inputs that are multi-line or contain shell-special characters should be sourced from files via `$(cat path)`.

## After dispatch — auto-notify

`orchestrated-dev` runs a terminal `Notify` step that dispatches the `notify` workflow before `End`. The notify workflow:

1. Captures the current branch (`git branch --show-current`).
2. Runs `cargo check --all-targets --message-format=json` to capture error + warning counts.
3. Computes a lint-suppression hygiene delta vs `origin/main` for touched `.rs` files: net `allow(dead_code)`, added `allow(unused_*)`, added panic-class lines (`unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`).
4. Sends a single DM to Waffles' and Tom's inboxes with branch, build counts, the audit numbers, and a `meridian stack land` next-step reminder.

Both `Done`'s success and failure routes go through `Notify`, so the DM fires regardless of whether the workflow itself succeeded or failed mid-flight. You don't need to wake-and-poll for completion — wait for the DM.

If you need to re-dispatch the notify workflow standalone (rare, e.g. after a failed dispatch): `meridian workflow run notify --workspace 2d5fdd51-1f25-45a4-8f86-4d4c978d1355 --as c9255b2a-5731-4d17-8124-e3bfa2224186 --input workflow=manual --input run-name="..."`. It runs in whatever cwd the dispatcher invoked it from, so cd into the worktree first if you want a meaningful diff.

## Monitoring

```bash
# List recent executions
meridian workflow history --workspace <id> --limit 10 --text

# Inspect a specific execution (summary + per-step)
meridian workflow status <execution-id> --text

# Filter history by status
meridian workflow history --workspace <id> --status running --text
meridian workflow history --workspace <id> --status failed --text

# Cancel a running workflow
meridian workflow cancel <execution-id>
```

## YAML Format

Workflow YAML lives in `.meridian/workflows/*.yaml`. Grammar matches the v1 `workflows` skill (see `.claude/skills/workflows/SKILL.md` for the full grammar — step kinds, templates, parsers). Two v2-specific rules worth knowing:

- `evaluate` step `criteria:` must be a **list of strings**, not a scalar. `criteria: "X"` is rejected; use `criteria:\n  - "X"`.
- A workflow's `output:` block is captured on completion and made available to parent dispatches as `{Step.field_name}`.

## Where to Put Things

- **Briefs** (workflow-ready requirement docs): `docs/design/briefs/` in the repo. NOT `/tmp`.
- **Design docs**: `docs/` or `docs/design/` in the repo. CLAUDE.md loads by default.
- **Workflow definitions**: `.meridian/workflows/*.yaml`.
- **Agent profiles**: `.meridian/profiles/*.yaml`.
- **Findings from a review**: temporary files inside the worktree are fine during a run, but anything durable goes in the repo.

## Recovery Patterns

- **Clippy exhausts AI budget during `orchestrated-dev`:** run `cargo clippy --fix` directly in the worktree, commit manually, then dispatch `review-and-land` against the same worktree with the remaining findings. Don't re-dispatch a fresh `orchestrated-dev` for mechanical clippy work.
- **Workflow fails mid-run:** inspect with `meridian workflow status <id> --text` first. The auto-notify DM will have fired regardless, but `status` gives you the per-step detail. Decide whether to resume with `review-and-land` against the existing worktree or cancel and start fresh.
- **403 on dispatch:** check that `--as <id>` resolves to a member with a reporting-tree link to the Yggdrasil workspace. Session IDs without that link are rejected.
- **Workflow not found:** check you're running against the yggdrasil repo — the server scans `.meridian/workflows/` relative to the repo root, not a user-level fallback.

## When to Use Which Workflow

- **Greenfield feature work from a brief:** `orchestrated-dev` with `--worktree --base main` and `--input run-name="..."`.
- **Apply reviewer findings on an existing branch:** `review-and-land` with `--worktree <existing-path>`.
- **Verify a branch without touching code:** `mechanical-review` or `smell-and-criterion-review`.
- **Stacked-branch land:** `stack-land`.

Discover the current set with `meridian workflow list --workspace 2d5fdd51-1f25-45a4-8f86-4d4c978d1355`.

## Differences From v1 (the `workflows` Skill)

| v1 (`shape workflow …`) | v2 (`meridian workflow …`) |
|---|---|
| `inspect <name>` | `show <name>` |
| `output <id>` / `output <id> <step>` / `--full` / `--json` | `status <id>` (flat: status + steps together) |
| `peek`, `pause`, `resume` | not available |
| `--worksite <name>` / `--worksite auto` | `--worktree [<PATH>]` (different semantics) |
| `--initiator ${CLAUDE_SESSION_ID}` | no flag; auto-notify DM is hard-wired to Waffles + Tom |
| Runs from v1 Meridian server (port 19876) | Runs from v2 Meridian server (port 29876) |

Do not mix. Running `shape workflow run` against the v2 workspace won't work; running `meridian workflow run` with v1 flags like `--worksite` or `--initiator` won't work either.
