# Aion-Workers — User Stories

## Python Activity Author — Writing ML / Data-Science Activities

**S1.** As a Python activity author, I want to decorate a normal async function with @activity and have its type hints drive input/output serialisation so that I can serve durable activities without learning Gleam, Rust, or the wire format.

**S2.** As a Python activity author, I want to heartbeat from a long-running inference job and observe cancellation so that the engine knows my job is alive and I can stop cleanly when the workflow is cancelled.

**S3.** As a Python activity author, I want to raise RetryableError or TerminalError so that the engine retries transient failures but does not retry a permanent one — and I want an unclassified exception to be treated as retryable so a network blip never permanently fails a workflow.

## TypeScript Activity Author — Writing Node-Ecosystem Activities

**S4.** As a TypeScript activity author, I want defineActivity<I, O> with generic input/output types so that my handler is type-checked and serialisation is handled at the boundary.

**S5.** As a TypeScript activity author, I want to install the SDK from npm with bundled type declarations so that it works out of the box in an ESM or CJS project.

## Rust Activity Author — Writing Isolated / Independently-Scaled Rust Activities

**S6.** As a Rust activity author, I want to register an async handler returning Result<Output, ActivityFailure> on a Worker builder and call Worker::run() so that out-of-process Rust activities serve a running engine with full durability semantics.

**S7.** As a Rust activity author, I want the SDK to depend on the wire contract (aion-proto) but not on the engine internals so that my worker binary stays small and decoupled from the engine.

## Worker Operator — Deploying and Scaling Workers

**S8.** As a worker operator, I want to configure the engine endpoint, task queue, identity, and max concurrency so that I can point a worker at the right engine and scale it without editing code.

**S9.** As a worker operator, I want a worker to reconnect automatically after a network drop and re-report any work it already finished so that a disconnect never loses a completed activity or double-runs a side effect.

**S10.** As a worker operator, I want a worker to shut down gracefully, draining in-flight activities before exiting so that a deploy does not abandon running work.

## Workflow Author — Consuming Activities Across Languages

**S11.** As a workflow author, I want an activity served by any language to behave identically — same completion, same retryable/terminal classification, same recorded result — so that I never need to know which runtime served it.

## SDK Maintainer — Keeping Three SDKs in Lockstep

**S12.** As an SDK maintainer, I want a cross-SDK conformance suite that drives all three SDKs against a fake engine harness so that I can prove they exhibit identical protocol behaviour and catch any drift between languages.
