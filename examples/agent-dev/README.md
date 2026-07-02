# agent-dev

A durable agent dev pipeline whose agent steps run through the norn harness —
observable and intervenable. This is the Phase-2 NOI dogfood workflow: one
flat workflow, six recorded activities, two bounded loops, honest terminal
dispositions.

## Flow

```
provision -> scout -> dev -> review
                       ^        |
                       +--------+   bounded by dev_review_cap (cumulative)
                     gate
                       |  fail -> dev(diagnostics) -> [bounded review loop] -> gate
                       |            bounded by gate_cap
                     land           (Passed ONLY)
```

- **provision** checks out `repo_url` at `base_ref` into a fresh branch for
  the brief and returns the workspace `{path, branch}`.
- **scout / dev / review** are agent activities under the norn-harness
  contract: ONE prompt string in, ONE terminal-text string out. The workflow
  composes every prompt (`src/agent_dev/prompts.gleam`): round one carries
  the full contract (brief + design notes + acceptance + scout plan), resume
  rounds are lean feedback-only prompts, because the worker pins one norn
  session per role per run.
- The **review verdict** is decoded defensively (`src/agent_dev/verdict.gleam`):
  the reviewer is instructed in-prompt to end with exactly
  `{"pass": bool, "blockers": [...], "summary": "..."}`; the workflow parses
  a trailing JSON object out of the terminal text, re-asks ONCE ("respond
  with only the JSON verdict") on an unparseable reply, and counts a still
  unparseable reply as a failed review round.
- **gate** runs the authoritative checks; a failing gate is recorded data,
  and each failure (budget permitting) drives a dev feedback round, a
  re-entry into the bounded review loop, and a re-gate.
- **land** merges the branch — dispatched ONLY on a `Passed` disposition.

Exhausting a cap is a **disposition** (`review_cap_exhausted` /
`gate_cap_exhausted`), never an error: the run completes, `land` is skipped,
and the workspace persists for inspection (its `branch` and `workspace_path`
ride the output). An `agent_dev_status` query answers live `{phase, round}`
at every stage.

Every input field is required — both caps are the caller's explicit budget;
nothing is defaulted.

## Layout

- `src/agent_dev.gleam` — the workflow (determinism boundary; entry is the
  one-line `workflow.entrypoint(definition(), raw_input)` shim).
- `src/agent_dev_io.gleam` — the authored boundary types (types-first,
  ADR-014). Edit a type, then run `aion generate .`.
- `src/agent_dev_codecs.gleam`, `schemas/*.json` — GENERATED artifacts.
  Do not edit; regenerate (`aion generate . --check` is the drift gate).
- `src/agent_dev/activities.gleam` — typed activity constructors; the
  scout/dev/review agent steps use a bare-string codec (prompt in, terminal
  text out).
- `src/agent_dev/prompts.gleam`, `src/agent_dev/verdict.gleam` — pure prompt
  composition and defensive verdict extraction.
- `inputs/CHIRON-RUFF-001.json` — the demo brief: add the missing ruff
  diagnostics adapter to chiron (compiled-vs-declarative left open on
  purpose).
- The **worker is a separate build**: it serves `provision`, `scout`, `dev`,
  `review`, `gate`, and `land`, driving the three agent roles through norn
  with one pinned session per role per run.

## Run it

```sh
gleam build
aion package . --build     # or: cargo run -p aion-cli -- package examples/agent-dev --build
aion server --config dev-config.toml   # the repo-root development config
aion deploy agent-dev.aion
# start the agent-dev worker (built separately), then:
aion start agent_dev --input "$(cat inputs/CHIRON-RUFF-001.json)"
aion query <workflow-id> agent_dev_status
aion describe <workflow-id> --pretty
```

## Tests

`gleam test` runs the whole pipeline hermetically under `aion/testing` with
scenario handlers registered per activity name: happy path, review-fail →
feedback → pass, gate-fail → dev round → pass, both cap exhaustions (gate
and land provably skipped), the cumulative-cap gate re-entry edge, and both
unparseable-verdict paths (re-ask recovery and failed-round accounting).
