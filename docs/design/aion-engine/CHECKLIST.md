# Aion-Engine — Checklist

## Crate Scaffold and Errors

- [ ] **C1** — The aion crate exists as a workspace member depending only on aion-core, aion-store, aion-package, and beamr among workspace crates, with unsafe_code = "deny".
- [ ] **C2** — src/lib.rs contains only pub mod declarations and pub use re-exports, no logic.
- [ ] **C3** — EngineError is a thiserror enum covering at least MissingStore, Load, Store(StoreError), Package(PackageError), Runtime, RegistryPoisoned, WorkflowNotFound, and NifRegistration.
- [ ] **C4** — No #[allow]/#[expect]/#[ignore] appears in the crate, and no .unwrap()/.expect() appears outside #[cfg(test)] code.

## beamr Runtime Embedding

- [ ] **C5** — Only the runtime module imports beamr; every other module reaches beamr exclusively through the RuntimeHandle.
- [ ] **C6** — The runtime is configured with a scheduler thread count taken from the builder; when unset, aion passes None so beamr applies its own default, and aion hardcodes no number.
- [ ] **C7** — RuntimeHandle exposes typed operations to spawn a workflow process, spawn a linked activity child, register a module, cancel a process by pid, and shut the runtime down.
- [ ] **C8** — A NIF registration surface accepts host-supplied NIFs plus the engine's own NIFs and registers them with beamr's native registry before any workflow runs; duplicate-MFA registration is surfaced as EngineError::NifRegistration.

## Active-Execution Registry

- [ ] **C9** — WorkflowHandle carries the beamr pid, the workflow type, the loaded version, and a cached WorkflowStatus.
- [ ] **C10** — The registry maps (WorkflowId, RunId) to a WorkflowHandle and supports insert, lookup, remove, and list of live handles.
- [ ] **C11** — The registry is concurrency-safe and lock poison is mapped to EngineError::RegistryPoisoned rather than panicking.
- [ ] **C12** — The cached status is reconciled against aion-core's event-history projection and the projection wins on disagreement; no code path sets status independently of events.

## Module Loading

- [ ] **C13** — load applies aion-package's (logical module name, content hash) -> deployed name transform to every module in a Package and registers the namespaced beams through the RuntimeHandle.
- [ ] **C14** — The loaded workflow version is recorded so start_workflow for that type spawns the entry module under its namespaced name.
- [ ] **C15** — Loading two packages with different content hashes registers two coexisting namespaced module sets with no name conflict.
- [ ] **C16** — A package whose entry module is absent, or whose namespaced module collides with an already-registered name from a different hash, fails with EngineError::Load and registers nothing.

## Supervision Tree

- [ ] **C17** — An engine supervisor is the root and one workflow supervisor is created per registered workflow type.
- [ ] **C18** — Each workflow execution runs as a process under its type's supervisor, and activity invocations run as linked child processes of the workflow process.
- [ ] **C19** — Workflow processes trap exits so an activity child crash arrives as a message, while activity processes do not trap exits.
- [ ] **C20** — Cancelling a workflow kills its linked activity children via link propagation with no manual teardown.

## In-VM Activity Dispatch

- [ ] **C21** — An in-VM activity is dispatched as a child BEAM process linked to the workflow process, running on the dirty scheduler when the underlying NIF is flagged dirty.
- [ ] **C22** — The activity's outcome — a result Payload or an exit signal carrying an ActivityError — is surfaced back to the workflow process through the link/mailbox.
- [ ] **C23** — Activity dispatch performs no event recording and makes no retry decision itself; those are delegated to the AD append path and AT retry machinery.

## Workflow Lifecycle

- [ ] **C24** — start_workflow assigns a WorkflowId and RunId, appends WorkflowStarted via the store, spawns the workflow process for the loaded type, registers the handle, and returns a WorkflowHandle.
- [ ] **C25** — Completion appends WorkflowCompleted with the result Payload, deregisters the handle, and unblocks result() awaiters; failure appends WorkflowFailed with the WorkflowError analogously.
- [ ] **C26** — cancel kills the workflow process (propagating to activity children), appends WorkflowCancelled with the reason, and deregisters the handle.
- [ ] **C27** — Suspend marks the handle Suspended when a workflow blocks on a durable wait, and resume returns it to Running when woken; the durable-wait mechanism itself is delegated to AT.

## Engine Embedding API

- [ ] **C28** — EngineBuilder offers new(), store(impl EventStore), scheduler_threads(n), load_workflows(...), register_nifs(...), and build().
- [ ] **C29** — build() wires the runtime, registers NIFs and loaded modules, repopulates the registry and re-creates supervisors from the store's list_active (delegating replay to AD), and returns EngineError::MissingStore if no store was supplied.
- [ ] **C30** — Engine exposes start_workflow, cancel, result, list_workflows, and shutdown.
- [ ] **C31** — list_workflows returns live handles from the registry and falls through to the store query for terminal workflows, returning WorkflowSummary values.
- [ ] **C32** — shutdown stops accepting new starts, lets in-flight store appends finish, drains and stops the runtime scheduler, and returns once beamr has stopped.
- [ ] **C33** — Engine surfaces signal, query, and subscribe whose registry-lookup step lives here while their delivery mechanics delegate to AT (signal routing, query dispatch) and AD/AT (event publishing).

## Engine Integration Tests

- [ ] **C34** — An integration test builds an Engine over InMemoryStore, starts a test workflow, and asserts WorkflowStarted is appended and the handle is registered and reported by list_workflows.
- [ ] **C35** — An integration test asserts cancelling a workflow kills its linked activity child and appends WorkflowCancelled, and that a completed workflow's result() returns the recorded Payload.
