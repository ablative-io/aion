# Aion-Core — User Stories

## Engine — Producing and Consuming Events

**S1.** As the engine, I want to append a batch of events atomically with an expected sequence so that concurrent or duplicate writes are detected and replay integrity is preserved.

**S2.** As the engine, I want to read a workflow's complete ordered history so that I can replay it to reconstruct the workflow's state.

**S3.** As the engine, I want to list active workflows so that on startup I know which workflows to replay and resume.

**S4.** As the engine, I want a failed activity's error to tell me whether it is retryable so that I apply the retry policy correctly instead of guessing.

## Store Author — Implementing a Persistence Backend

**S5.** As a store author, I want a single clear EventStore trait so that there is exactly one reasonable way to implement a backend correctly.

**S6.** As a store author, I want an in-memory reference implementation and a shared behavioural test suite so that I can verify my backend matches the contract exactly.

**S7.** As a store author, I want to implement the trait without depending on the engine so that my backend crate stays decoupled.

## Workflow Author — Writing Workflows via the SDK

**S8.** As a workflow author, I want time and identifiers to be deterministic so that my workflow replays to exactly the same state every time.

## Operator — Observing Workflows

**S9.** As an operator, I want to query workflows by type, status, and time range so that I can see what is running and what has failed.

**S10.** As an operator, I want a lightweight summary per workflow so that listing many workflows does not require loading every full history.

## Worker — Implementing Activities

**S11.** As a worker, I want to depend on the domain types without pulling in the store trait so that my worker binary stays small and decoupled.
