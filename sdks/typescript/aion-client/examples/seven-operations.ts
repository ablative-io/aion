import {
  AlreadyExistsError,
  QueryTimeoutError,
  UnavailableError,
  connect,
} from "../src/index.js";

type EchoState = {
  readonly lastSignal?: string;
};

async function run(): Promise<void> {
  const endpoint =
    process.argv[2] ?? process.env.AION_SERVER_URL ?? "http://127.0.0.1:8080";
  const bearerToken = process.env.AION_AUTH_TOKEN;
  const client = await connect({
    endpoint,
    namespace: "conformance",
    auth:
      bearerToken === undefined
        ? undefined
        : { bearerToken, namespaces: ["conformance"] },
    tls: {
      enabled: process.env.AION_INSECURE !== "1" && endpoint.startsWith("https://"),
    },
  });

  const handle = await client.start({
    workflowType: "conformance.echo",
    input: { message: "hello", counter: 1 },
    idempotencyKey: `aion-client-typescript-seven-operations-${process.pid}`,
  });
  console.log(
    `started workflow=${handle.workflowId} run=${handle.runId ?? "latest"}`,
  );

  await handle.signal({
    signalName: "record",
    payload: { value: "signal-observed" },
  });
  console.log("sent signal record");

  const state = await handle.query<undefined, EchoState>({
    queryName: "state",
    timeoutMs: 5_000,
  });
  console.log("query state", state);

  const { summaries } = await client.list({ namespace: "conformance" });
  console.log(`listed ${summaries.length} workflow(s)`);

  const description = await handle.describe({ includeHistory: true });
  console.log(`described ${description.history.length} event(s)`);

  await handle.cancel({
    reason: "seven-operations example requested cancellation",
  });
  console.log("cancel requested");

  for await (const event of handle.subscribe({ maxReconnects: 3 })) {
    console.log(`subscribed event seq=${event.seq}`);
    break;
  }
}

try {
  await run();
} catch (error) {
  if (error instanceof UnavailableError) {
    console.error(
      "aion-server is unavailable; check AION_SERVER_URL and the fixture",
    );
  } else if (error instanceof AlreadyExistsError) {
    console.error("idempotency key was reused for a different start request");
  } else if (error instanceof QueryTimeoutError) {
    console.error("query timed out before the fixture replied");
  } else {
    throw error;
  }
  process.exitCode = 1;
}
