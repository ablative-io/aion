# Aion-Store-Libsql — User Stories

## Engine Operator — Deploying Aion with Durable Storage

**S1.** As an engine operator, I want a zero-infrastructure durable store I get by pointing at a file path, so that embedding Aion needs no database server to survive restarts.

**S2.** As an engine operator, I want the same store crate to drive a distributed deployment via an embedded replica syncing to a remote primary, so that I do not have to learn or wire a second backend to scale out.

**S3.** As an engine operator, I want to tune libSQL durability and sync settings through configuration with no values silently assumed for me, so that I control the durability-versus-throughput trade-off for my deployment.

## Engine — Persisting and Replaying Workflow State

**S4.** As the engine, I want append to be atomic and gated on an expected sequence over durable storage, so that concurrent or duplicate writes are rejected and replay integrity is preserved across crashes.

**S5.** As the engine, I want a workflow's complete history to survive process restarts so that I can replay it from durable storage after a crash or reboot.

**S6.** As the engine, I want to list active workflows and durable timers from storage on startup so that I know what to replay and which timers are due after downtime.

## Store Author — Maintaining the libSQL Backend

**S7.** As a store author, I want this backend to pass the shared behavioural conformance suite unmodified so that I have objective proof it matches the in-memory oracle exactly.

**S8.** As a store author, I want every libSQL and serialisation error mapped into the StoreError taxonomy so that the engine handles one uniform error surface regardless of backend.

**S9.** As a store author, I want libSQL's internals to be swappable for the Turso-native engine later without touching the engine, so that adopting the production-ready rewrite is a drop-in change when it leaves beta.

## Operator — Observing Workflows

**S10.** As an operator, I want to query durable workflows by type, status, and time range so that I can see what is running and what has failed without loading every full history.
