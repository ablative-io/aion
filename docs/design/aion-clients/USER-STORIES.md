# Aion-Clients — User Stories

## Application Developer — Driving Workflows from Application Code

**S1.** As an application developer, I want to add a client dependency, point it at a server URL, and start a workflow by type name with a typed input so that I can trigger durable work from my service without learning the engine internals.

**S2.** As an application developer, I want start to return a handle that lets me signal, query, cancel, and describe the workflow I just started so that I do not have to plumb IDs through my own code.

**S3.** As an application developer, I want start to accept an idempotency key so that retrying a failed start from my own retry loop does not launch the workflow twice.

**S4.** As an application developer, I want a typed result back from a query and a typed input accepted by a signal so that my compiler catches payload-shape mistakes before runtime.

## Operator — Operating Running Workflows

**S5.** As an operator, I want to list workflows by type, status, and time range so that I can see what is running and what has failed.

**S6.** As an operator, I want to describe one workflow and read its status and history so that I can diagnose where it is stuck.

**S7.** As an operator, I want to cancel a run with a reason and understand that it is a cooperative request, not an immediate kill, so that I do not assume the workflow stopped the instant the call returned.

## Dashboard / Observability Developer — Streaming Live Events

**S8.** As a dashboard developer, I want to subscribe to a workflow's live event stream and have the client resume transparently across transient disconnects so that I render a gap-free, duplicate-free timeline without writing reconnection logic.

**S9.** As a dashboard developer, I want a terminal stream failure to surface as an error through the stream rather than a silent end so that I can distinguish a finished subscription from a broken one.

## Polyglot Platform Team — Consuming Aion from Multiple Languages

**S10.** As a platform team, I want the Python, TypeScript, Gleam, and Rust clients to behave identically against the same server so that teams in different languages can reason about Aion the same way.

**S11.** As a platform team, I want each client to map server failures to a documented, branchable error taxonomy so that my code can react differently to an idempotency conflict, a query timeout, and an auth failure.

**S12.** As a platform team, I want each client published to its native registry with a README and a runnable example so that adopting it is a one-line dependency add.

## Embedded Integrator — Calling an In-Process Engine

**S13.** As an embedded integrator, I want the Rust client to work against an in-process aion engine through the same surface as the network client so that I can drive workflows without standing up a server, and without pulling the engine into network-only consumers.
