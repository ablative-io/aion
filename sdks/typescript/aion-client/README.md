# @aion/client

TypeScript caller SDK for connecting to an `aion-server` deployment and operating workflows from Node.js or browser runtimes. It exposes connect plus the seven workflow operations: `start`, `signal`, `query`, `cancel`, `list`, `describe`, and `subscribe`.

## Install

```sh
npm install @aion/client
```

## Server prerequisite

Run an `aion-server` that implements the AW workflow API. The runnable example uses the AL-007 fixture defaults:

```sh
export AION_SERVER_URL=http://127.0.0.1:8080
export AION_AUTH_TOKEN=dev-token # optional
npx tsx examples/seven-operations.ts
```

See [`examples/seven-operations.ts`](examples/seven-operations.ts) for a complete program covering all seven operations. Subscribe requires a server stream transport adapter; pass it as `streamTransport` when constructing the client.

## Connect

```ts
import { connect } from "@aion/client";

const client = await connect({
  endpoint: process.env.AION_SERVER_URL ?? "http://127.0.0.1:8080",
  namespace: "conformance",
  auth: process.env.AION_AUTH_TOKEN
    ? { bearerToken: process.env.AION_AUTH_TOKEN, namespaces: ["conformance"] }
    : undefined,
  tls: { enabled: false },
});
```

## start

Generic JSON payloads are the typed path. `idempotencyKey` makes caller retries safe: identical retry returns the original handle, conflicting reuse raises `AlreadyExistsError`.

```ts
type StartInput = { message: string; counter: number };

const handle = await client.start<StartInput>({
  workflowType: "conformance.echo",
  input: { message: "hello", counter: 1 },
  idempotencyKey: "readme-seven-operations",
});
```

## signal

```ts
await handle.signal({
  signalName: "record",
  payload: { value: "signal-observed" },
});
```

## query

The query result is decoded into the generic result type. The current AW query request has no argument payload field, so omit `input` until the server wire type adds it.

```ts
type EchoState = { lastSignal?: string };

const state = await handle.query<undefined, EchoState>({
  queryName: "state",
  timeoutMs: 5_000,
});
```

## list

```ts
const { summaries } = await client.list({ namespace: "conformance" });
console.log(`listed ${summaries.length} workflow(s)`);
```

## describe

```ts
const description = await handle.describe({ includeHistory: true });
console.log(description.summary, description.history.length);
```

## cancel

Cancellation is a cooperative request: success means the server accepted the request.

```ts
await handle.cancel({ reason: "caller requested cancellation" });
```

## subscribe

`handle.subscribe()` returns an `AsyncIterable<WorkflowEvent>`. It reconnects after transient `Unavailable` failures using the last delivered per-workflow sequence number; terminal failures are thrown from iteration.

```ts
for await (const event of handle.subscribe({ maxReconnects: 3 })) {
  console.log(event.seq, event.envelope);
  break;
}
```

## Typed and raw payloads

Typed operations serialize JSON values by default. The raw escape hatch is always available through `rawPayload`, `startRaw`, `signalRaw`, and `queryRaw`:

```ts
import { rawPayload } from "aion-client-typescript";

await handle.signalRaw({
  signalName: "record",
  payload: rawPayload(new TextEncoder().encode('{"value":"raw"}'), "application/json"),
});
```

## Branching on errors

Errors are branchable subclasses and also carry a discriminating `kind`.

```ts
import {
  AlreadyExistsError,
  QueryTimeoutError,
  UnavailableError,
} from "aion-client-typescript";

try {
  await handle.query<undefined, EchoState>({ queryName: "state", timeoutMs: 10 });
} catch (error) {
  if (error instanceof QueryTimeoutError) {
    console.error("query timed out; use a longer timeout");
  } else if (error instanceof AlreadyExistsError) {
    console.error("idempotency key was reused for a different start");
  } else if (error instanceof UnavailableError) {
    console.error("server or stream transport is unavailable");
  } else {
    throw error;
  }
}
```
