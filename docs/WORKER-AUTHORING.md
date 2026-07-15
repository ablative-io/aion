# Worker Authoring Guide

How to write an Aion activity worker, grounded in the proven reference
implementation: `examples/dev-brief/worker/`. Read that package alongside this
guide — every pattern named here is implemented there, and when this guide and
that code disagree, the code wins.

## What a worker is

A worker is a standalone binary that dials the Aion server's liminal listen
address (default `127.0.0.1:50061` — the server config's
`[outbox] liminal_listen_address`), registers itself, and serves **activity
handlers**. The server pushes dispatched activities to it; the worker executes
them and returns JSON results that land in durable workflow history.

Workers are consumed by workflow documents (`.awl` files, or Gleam-authored
workflows). The document declares actions with typed inputs/outputs and pins
each action to a node; the worker must serve an activity type of the same name,
on that node, speaking JSON that matches the declared types.

## The routing model (get this right first)

The server routes a dispatched activity by exactly three dimensions:

    namespace × task_queue × node

**Never by activity type.** Consequences:

- One binary may hold several liminal connections; each connection registers
  ONE node. Every activity the document pins to that node lands on that
  connection — so a connection must serve every activity type its node owns.
- If two handlers must not share a process-level resource (for example, two
  differently-composed agent harnesses), put them on separate connections with
  distinct nodes, and pin the document's actions accordingly.
- The node strings in the worker are load-bearing: they must equal the node
  names the workflow document declares (`node developer` in AWL). dev-brief
  keeps an authoritative node table and asserts the constants match it.

dev-brief's topology: nine activity types on ONE task queue (`dev_brief`),
across THREE connections in one process — `developer` (driven agent),
`reviewer` (driven agent), `shell` (seven mechanical handlers).

## Package shape

Model the package on `examples/dev-brief/worker/`:

- **Standalone crate, NOT a workspace member** (`[workspace]` empty table in
  its `Cargo.toml`) so it builds with a plain `cargo build` in its directory.
  Aion crates are consumed by path:
  `aion-worker` (with the `liminal-transport` feature — this enables the
  server-push serve path), `aion-integrations`, and `aion-integration-norn`
  for driven-agent workers.
- `[[bin]]` + `[lib]` split: the binary is a thin composition root
  (`main.rs`); everything testable lives in the library.
- Lints: deny `unsafe_code`; clippy `all` + `pedantic` warn;
  `unwrap_used`/`expect_used`/`panic`/`todo` warn. In practice the bar is the
  house bar: **no unwrap/expect/panic anywhere including tests, no
  `#[allow]`/`#[expect]`/`#[ignore]`, files ≤ 500 code lines, `mod.rs` holds
  declarations and re-exports only, backticked identifiers in doc comments.**
- Builds go to the package's own `target/` (or a repo-level shared
  `CARGO_TARGET_DIR`). **Never build into `/tmp`.** Logs and artifacts go to
  durable paths, never `/tmp`.

## The two kinds of activities

### 1. Mechanical (shell-node) activities

Plain handlers in a typed `ActivityRegistry`. Write the bodies as synchronous
`(&Shell, Input) -> Result<Output, ActivityFailure>` functions so hermetic
tests drive them directly; `main.rs` adapts them onto the async handler
signature.

**The failure taxonomy — the single most important discipline:**

- A command that cannot RUN at all (missing executable, dead working
  directory) is an INFRASTRUCTURE failure → terminal `ActivityFailure`.
  Retrying a broken environment cannot help.
- A command that runs and exits non-zero is a CONTRACT VERDICT → **recorded
  data returned as a successful activity result**, never an error. The exit
  status and captured output ride back into durable history so the workflow
  can branch on them. Nothing is ever swallowed into a success; nothing real
  is ever hidden inside an error.

**The shell boundary** (`shell.rs` in dev-brief — reuse it, don't reinvent):
`Shell::inherited()` resolves executables against the process `PATH`;
`Shell::with_path(dir)` resolves against exactly one directory. Every run
captures exit status (`128 + signal` for signal deaths), stdout alone (for
JSON parsing — stderr tails corrupt parses), combined output (for humans), and
duration. This split is what makes the hermetic test pattern work (below).

### 2. Driven agent activities

An agent activity runs a real coding agent (Norn) as a driven session. The
wiring:

- `NornHarness` (from `aion-integration-norn`) carries the static per-role
  arguments: `--append-system-prompt <doctrine>` (**never** `--system-prompt`,
  which would overwrite Norn's own instructions), `--output-schema <json>`,
  `--session-id <template>`, `--resume-if-exists`, optional
  `--disallowed-tools`, and env hygiene.
- A thin wrapper (dev-brief's `ProfiledNornHarness`) intercepts ONE seam: it
  reads the activity input's payload, assembles the per-turn prompt from it,
  and reads per-run values (like `workspace_path` → a per-run
  `--workspace-root`). Everything static stays on the inner harness.
- The composed harness is erased to `Arc<dyn DynAgentHarness>` and served via
  `AgentHarnessConfig::new(erased, [activity_type], capabilities)` with
  intervention capabilities (`InjectMessage`, `Cancel`) so the ops console can
  intervene.

**Session identity is the superpower:** `--session-id` templated on
`{workflow_id}` plus `--resume-if-exists` means an activity called twice in
one workflow RESUMES the same agent conversation — dev-brief's developer keeps
its context across fix cycles this way. Choose session keys deliberately;
they define which calls share memory.

**Env hygiene:** the harness removes `OPENAI_API_KEY` from Norn's child
environment (`.without_env(...)`) so Norn uses the operator's ChatGPT OAuth
login. **No secret is ever read, stored, printed, or passed by a worker.**
Env var names are fine; values never.

**Agents do not run git — the machinery does.** Where an agent produces
commits-worth of work, a post-run hook commits mechanically under a machinery
identity and rewrites the agent's asserted commit hashes to the real head
(agents fabricate hashes; reality wins). See `harness.rs`/`commit.rs`.

## Serving

Per connection: build a `WorkerConfig` (builder: `endpoint` — the literal
`"unused-direct-address"` when dialling candidates, `namespace`, `task_queue`,
`node`, `identity`, `max_concurrency`, reconnect backoffs with
`usize::MAX` attempts — a long-lived worker must outwait server restarts),
then call `aion_worker::serve_with_redial(candidates, &config, &registry,
RedialTiming::new(..), &stop, agent_harness_or_none, on_registered)`.

Spawn one thread per agent connection; serve the shell connection on the main
thread and write a **readiness file** in its `on_registered` callback so
supervisors can await "connected". Standard CLI surface (mirror dev-brief's
`main_args.rs`): `--address` (repeatable candidates; default
`127.0.0.1:50061`), `--identity`, `--ready-file`, `--norn-bin` (or `NORN_BIN`
env), plus whatever the package needs. Unknown flags are a loud error.

Workers are started **manually or by Aion-native supervision — never add new
launchd jobs.**

## Matching the workflow document

- Activity type strings, node names, and the task queue must agree between
  the document and the worker. dev-brief cross-references them against one
  authoritative table; do the same.
- Inputs/outputs are JSON. The document's declared types compile to JSON
  codecs; the worker `serde`-parses the input envelope and must return JSON
  the declared output type decodes. Field-name or shape drift = a runtime
  decode failure in the workflow, so pin the contract with tests on both
  sides where possible.
- Check documents with `aion awl check`; a worker package should ship at
  least one example document proving the contract end to end.

## Testing (the hermetic pattern)

The proven seam: construct `Shell::with_path(shim_dir)` where `shim_dir`
contains **fake CLI shims alone** (tiny scripts named `git`, `cargo`, `norn`,
…). The handlers really shell out; the shims intercept at the process
boundary — the most realistic seam that needs no network and no real repos.
See `worker/tests/shell_activities.rs` and the harness tests.

Test the composition too: argument parsing via an injected iterator (no
process globals), harness argv assembly (assert the exact composed arguments),
and every failure-taxonomy branch (unrunnable → terminal; non-zero → data).

Run the package's own gate battery (`cargo build`, `cargo clippy`,
`cargo test`, `cargo fmt`) with **full output redirected to files and an
exit-code manifest** — never pipe cargo output through head/tail/grep.

## Checklist

1. Topology decided: activity types → nodes → connections; names match the
   document's table.
2. Standalone package modeled on dev-brief; house lints and file-size laws.
3. Mechanical handlers: sync bodies, `Shell` boundary, failure taxonomy.
4. Agent handlers: inner harness (static args) + wrapper (per-run seam),
   deliberate session keys, env hygiene, intervention capabilities.
5. `serve_with_redial` per connection; readiness file; manual start.
6. Hermetic tests with CLI shims; argv-assembly tests; taxonomy tests.
7. Example document + `aion awl check` green; gate battery green with
   manifest.
