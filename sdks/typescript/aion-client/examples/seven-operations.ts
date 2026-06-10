import {
  AlreadyExistsError,
  QueryTimeoutError,
  type SubscribeRequest,
  type SubscribeTransport,
  UnavailableError,
  connect,
} from "../src/index.js";

type EchoState = {
  readonly lastSignal?: string;
};

class WebSocketSubscribeTransport implements SubscribeTransport {
  constructor(
    private readonly endpoint: string,
    private readonly bearerToken: string | undefined,
  ) {}

  async *subscribe(request: SubscribeRequest): AsyncIterable<unknown> {
    if (request.resumeFrom !== undefined) {
      throw new UnavailableError(
        "resume_from is not yet supported by the server wire protocol",
      );
    }
    if (this.bearerToken !== undefined) {
      throw new UnavailableError(
        "the example WebSocket transport cannot attach authorization headers",
      );
    }
    const socket = new WebSocket(eventsEndpoint(this.endpoint));
    try {
      await socketOpen(socket);
      socket.send(
        JSON.stringify({
          per_workflow: {
            namespace: request.namespace,
            workflow_id: { uuid: request.workflowId },
          },
        }),
      );

      while (true) {
        const frame = await socketMessage(socket);
        if (frame === undefined) {
          return;
        }
        yield frame;
      }
    } finally {
      socket.close();
    }
  }
}

function eventsEndpoint(endpoint: string): string {
  const parsed = new URL(endpoint);
  parsed.protocol = parsed.protocol === "https:" ? "wss:" : "ws:";
  parsed.pathname = "/events/stream";
  parsed.search = "";
  parsed.hash = "";
  return parsed.toString();
}

function socketOpen(socket: WebSocket): Promise<void> {
  return new Promise((resolve, reject) => {
    const cleanup = () => {
      socket.removeEventListener("open", onOpen);
      socket.removeEventListener("error", onError);
    };
    const onOpen = () => {
      cleanup();
      resolve();
    };
    const onError = () => {
      cleanup();
      reject(new UnavailableError("event stream connection failed"));
    };
    socket.addEventListener("open", onOpen);
    socket.addEventListener("error", onError);
  });
}

function socketMessage(socket: WebSocket): Promise<unknown | undefined> {
  return new Promise((resolve, reject) => {
    const cleanup = () => {
      socket.removeEventListener("message", onMessage);
      socket.removeEventListener("close", onClose);
      socket.removeEventListener("error", onError);
    };
    const onMessage = (event: MessageEvent) => {
      cleanup();
      resolve(normalizeSocketFrame(event.data));
    };
    const onClose = () => {
      cleanup();
      resolve(undefined);
    };
    const onError = () => {
      cleanup();
      reject(new UnavailableError("event stream receive failed"));
    };
    socket.addEventListener("message", onMessage);
    socket.addEventListener("close", onClose);
    socket.addEventListener("error", onError);
  });
}

async function normalizeSocketFrame(frame: unknown): Promise<unknown> {
  if (frame instanceof Blob) {
    return frame.text();
  }
  if (frame instanceof ArrayBuffer) {
    return new TextDecoder().decode(frame);
  }
  return frame;
}

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
    streamTransport: new WebSocketSubscribeTransport(endpoint, bearerToken),
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
