# @aion/worker

TypeScript/Node remote-worker SDK for registering out-of-process Aion activities and serving them from an `aion-server` task queue.

## Install

```sh
npm install @aion/worker
```

## Minimal worker

Define activities, pass them to a worker, and start the async run loop. Provide either a `clientFactory` for the gRPC transport or a custom `sessionFactory` for your runtime; the example below uses a placeholder factory for brevity.

```ts
import { Worker, defineActivity, type WorkerSessionFactory } from "@aion/worker";

const greet = defineActivity<{ name: string }, { message: string }>(
  "examples.greet",
  async (input) => ({ message: `hello, ${input.name}` }),
);

const config = {
  endpoint: "http://127.0.0.1:50051",
  taskQueue: "default",
  identity: "typescript-worker-1",
  maxConcurrency: 8,
  reconnect: {
    initialDelayMs: 100,
    maxDelayMs: 5_000,
    maxAttempts: 10,
  },
};

const sessionFactory: WorkerSessionFactory = async (workerConfig) => {
  // Return a WorkerSession connected to workerConfig.endpoint for your transport.
  throw new Error(`connect WorkerSession for ${workerConfig.endpoint}`);
};

await new Worker(config, [greet], { sessionFactory }).run();
```

See the main Aion repository at <https://github.com/ablative-io/aion>.
