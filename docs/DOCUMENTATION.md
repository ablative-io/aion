# Aion — Documentation

## What is Aion?

Aion is a workflow engine — it runs multi-step processes that don't lose their place. If your app needs to do something that takes minutes, hours, or months (process an order, run an approval chain, coordinate between services), Aion keeps track of every step. If the server crashes mid-way through, Aion picks up exactly where it left off when it restarts. No lost work, no half-finished processes, no manual recovery.

Think of it like a bookmark that never falls out. A workflow can pause for three months waiting for someone to approve something, survive server restarts, and resume the moment the approval arrives — without anyone having to remember where it was up to.

The name comes from ancient Greek — Aion is the concept of eternal, unbounded time, as opposed to Chronos (sequential, ticking time). A workflow that sleeps for months and resumes is living in eternal time.

## Why does Aion exist?

Most apps handle multi-step processes with fragile chains — a queue here, a cron job there, a database flag to track progress. When something fails mid-way, you're left debugging what happened and manually pushing things forward.

Aion replaces all of that with one system:

- **Crash-proof execution** — Every step is recorded. If the server dies mid-workflow, it replays the history on restart and resumes at the exact point it stopped. Completed steps aren't re-run — their results are replayed from the record.
- **Long-running workflows** — A workflow can wait for days or months (for an approval, an external event, a timer) without consuming resources while it waits. It just parks and resumes when the signal arrives.
- **Typed workflows** — Workflows are written in Gleam, a type-safe language. The inputs, outputs, and messages between steps are checked at compile time, so type mismatches are caught before anything runs.
- **Versioned deploys** — Every workflow package gets a content-hash version. Deploy a new version to a running server without stopping it. Running workflows keep their original version; new starts use the new one. Roll back atomically if something's wrong.
- **One CLI for everything** — Install one tool (`aion`), and you can run the server, package workflows, deploy them, start runs, send signals, and query status. No separate binaries, no complex setup.

## How does Aion fit in the Ablative Stack?

Aion is the workflow layer — the part that coordinates multi-step processes:

```
Your application (web app, service, agent)
        ↓
Aion orchestrates the steps and keeps track of progress
        ↓
Liminal delivers messages between steps
        ↓
Haematite stores workflow state and history
        ↓
Beamr runs the workflow code
        ↓
Lys records every action for audit
```

When Norn (the agent runtime) needs an AI agent to follow a multi-step process — research, draft, review, revise, publish — Aion is what ensures each step completes and the process survives interruptions.

## Current Status

**Active development, working today** — Aion has a working CLI, server, Gleam workflow SDK, and worker SDKs in Rust, Python, and TypeScript. The core durability engine (event-sourced replay, durable timers, signals, queries, child workflows) is working. The monitoring dashboard UI is under development.

**Honest limits:** Aion is currently single-node — one server process owns the store. There's no clustering yet. Activity retries are workflow-driven (your workflow code controls retry logic), not automatic from the engine.

## Getting Started

### What you'll need

- **Rust** — Install from [rustup.rs](https://rustup.rs). Aion's CLI and server are built in Rust.
- **Gleam** — Install from [gleam.run](https://gleam.run/getting-started/installing/). Workflows are written in Gleam. You'll also need Erlang/OTP on your PATH (Gleam's installer will guide you).

### Install the CLI

```bash
cargo install aion-cli --locked
```

This installs the `aion` command — the one tool you need. It runs the server, packages workflows, deploys them, and operates runs. Verify it works:

```bash
aion --help
```

### Scaffold a project

The fastest way to start is with `aion new`, which generates a complete, buildable project:

```bash
aion new my-workflow --template hello-world
```

This creates everything you need: a Gleam workflow, JSON schemas, a packaging descriptor, a dev server config, and a README with instructions. Four templates are available:

- `hello-world` — a single workflow with one activity, one signal, and one query
- `approval-flow` — a workflow that waits for human approval
- `saga` — a multi-step process with compensation (undo) logic
- `dev-pipeline` — three composed workflows with generated codecs and a test suite (add `--worker rust` for a pre-built worker crate)

### Build and run

From your scaffolded project:

```bash
gleam build              # compile the workflow
aion package .           # create a deployable .aion package
aion server --config aion.toml   # start the server (in one terminal)
aion deploy my-flow.aion         # deploy the package (in another terminal)
aion start my_flow --input '{"name": "Ada"}'   # start a workflow run
```

### Interact with a running workflow

```bash
aion query <workflow-id> status                              # ask the workflow where it's at
aion signal <workflow-id> approval --payload '{"approver": "ada"}'  # send it a signal
aion describe <workflow-id> --pretty                         # see the full run history
```

### Prove the durability

While a workflow is waiting for a signal, kill the server (`kill -9`), restart it (`aion server --config aion.toml`), and query the workflow again. It's still there, still waiting, right where you left it. The activity that already completed isn't re-run — its result is replayed from the recorded history.

## Key Concepts

**Workflows** — The process definition. Written in Gleam using the `aion_flow` SDK. A workflow describes the steps, their order, what data flows between them, and what to do when things go wrong. Workflows are deterministic — given the same history, they always produce the same result.

**Activities** — The actual work. Activities run on workers you write and control (in Rust, Python, or TypeScript). They're the only part that talks to the outside world — calling APIs, reading databases, sending emails. If an activity fails, the workflow decides what to do (retry, compensate, abort).

**Signals** — Messages sent to a running workflow from outside. "The user approved the request." "The payment cleared." "Cancel this order." The workflow can wait durably for a signal — for seconds or months.

**Queries** — Questions you can ask a running workflow without changing it. "What status are you at?" "How many items have you processed?" Queries read state but never modify it.

**Workers** — Programs you write that execute activities. A worker connects to the Aion server, picks up activity tasks, runs them, and returns results. You write workers against a worker SDK — they're your code, running in your environment, with access to your systems.

**Packages** — Deployable workflow bundles (`.aion` files). Each package gets a content-hash version — the same workflow code always produces the same version string. Deploy to a running server without downtime; roll back to a previous version with one command.

## Learn More

- [Getting started guide](https://github.com/AblativeOrg/aion/blob/main/docs/GETTING-STARTED.md) — Full walkthrough: build every piece by hand so you understand how they fit together
- [Ablative Stack overview](https://ablative.com.au/stack) — See how Aion fits with the other components

## License

AGPL-3.0 — free to use and modify. If you distribute a modified version or run it as a service, you must share your changes under the same license.
