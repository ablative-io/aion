# Aion-Engine — User Stories

## Engine Operator — Embedding and Running the Engine

**S1.** As an engine operator, I want to build an Engine from a store, loaded workflows, and my NIFs with one builder so that embedding durable workflows in my Rust app is a single, clear assembly step.

**S2.** As an engine operator, I want to set the scheduler thread count (and have a sensible runtime default when I don't) so that I tune concurrency to my host without the engine guessing a number for me.

**S3.** As an engine operator, I want a graceful shutdown that finishes in-flight appends and stops the runtime cleanly so that stopping the engine never abandons writes or leaks scheduler threads.

**S4.** As an engine operator, I want the engine to depend on no network stack so that I can run durable workflows in a CLI or single service with nothing but a file-backed store.

## Workflow Caller — Starting, Cancelling, and Inspecting Workflows

**S5.** As a workflow caller, I want to start a workflow by type and get a handle so that I can later await its result or cancel it by ID.

**S6.** As a workflow caller, I want to cancel a running workflow and have its in-flight activities torn down automatically so that cancellation leaves no orphaned work.

**S7.** As a workflow caller, I want to list and query workflows by type, status, and time so that I can see what is running and what has finished without loading full histories.

**S8.** As a workflow caller, I want to await a workflow's result and receive either its output payload or its terminal error so that I can react to the outcome.

## Workflow Deployer — Deploying Workflow Packages

**S9.** As a workflow deployer, I want to load a .aion package and have its modules registered under content-hash-namespaced names so that multiple versions coexist and a long-running execution keeps the exact module set it started on.

**S10.** As a workflow deployer, I want a corrupt or entry-less package to be rejected on load with nothing partially registered so that a bad deploy never half-installs a workflow.

## Reliability Engineer — Relying on Supervision and Fault Isolation

**S11.** As a reliability engineer, I want an activity crash to reach the workflow process as a trapped exit rather than killing it outright so that the workflow can apply its retry policy instead of dying.

**S12.** As a reliability engineer, I want workflows supervised per type under an engine root so that crashes are isolated and the supervision shape is explicit and bounded.
