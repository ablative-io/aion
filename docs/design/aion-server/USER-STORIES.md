# Aion-Server — User Stories

## Client SDK Author — Building a caller SDK (cluster AL) against the wire contract

**S1.** As a client SDK author, I want one published wire contract with gRPC service definitions so that I can generate stubs for my language instead of inventing my own request and response shapes.

**S2.** As a client SDK author, I want failures to arrive as stable error codes so that my SDK can branch on 'workflow not found' versus 'namespace denied' without parsing strings.

## Worker SDK Author — Building a remote-worker SDK (cluster AR) against the wire contract

**S3.** As a worker SDK author, I want the worker protocol defined in the shared contract so that registration, task dispatch, results, and heartbeats are identical across every language.

**S4.** As a worker SDK author, I want my build to depend only on the wire contract and the domain types, never the engine, so that my worker binary stays small and decoupled.

## Workflow Caller — Driving workflows over the network

**S5.** As a workflow caller, I want to start, signal, query, cancel, list, and describe workflows over gRPC or HTTP so that I can drive the engine from any service without embedding it.

## Dashboard / Observer — Watching workflow execution in real time

**S6.** As an observer, I want to open a WebSocket and subscribe to one workflow, a filtered class, or the whole firehose so that I see events as they happen.

**S7.** As an observer on a slow connection, I want to be dropped with a clear lag signal so that my slowness never stalls the engine or other subscribers, and I know to resubscribe.

**S8.** As a dashboard, I want to be served by the server and backed by the same public API every client uses so that there is no privileged side channel to maintain.

## Remote Worker — Executing Tier-3 activities out of process

**S9.** As a remote worker, I want to connect once, advertise the activity types I implement, and have tasks pushed to me so that I run heavy work without polling.

**S10.** As a remote worker running a long activity, I want to heartbeat so that the engine knows I am alive and does not treat my task as failed.

## Engine Operator — Deploying and running the standalone server

**S11.** As an operator, I want a single binary I point clients and workers at, configured by file/env with no baked-in defaults, so that I can run a managed deployment without embedding the engine in my own code.

**S12.** As an operator running many tenants, I want each namespace isolated so that a caller or worker in one namespace can never touch another's workflows, events, or tasks.

**S13.** As a security-conscious operator, I want credentials in config to be load-only so that a DSN, token, or TLS key can never be accidentally re-serialised into a log, error, or response.
