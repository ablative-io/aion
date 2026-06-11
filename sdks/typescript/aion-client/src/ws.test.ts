import assert from "node:assert/strict";
import type { IncomingMessage } from "node:http";
import test from "node:test";
import { type WebSocket as ServerSocket, WebSocketServer } from "ws";
import {
  InvalidArgumentError,
  NamespaceDeniedError,
  NotFoundError,
  ServerError,
  UnavailableError,
} from "./errors.js";
import { decodeWorkflowEvent, eventStream } from "./stream.js";
import {
  EVENT_STREAM_PATH,
  WebSocketSubscribeTransport,
  decodeStreamFrame,
  eventStreamUrl,
  subscriptionRequestFrame,
} from "./ws.js";

const WORKFLOW_UUID = "0b54f48a-9d12-4f43-9ce4-2c4c8e2c2f01";

test("eventStreamUrl maps the HTTP endpoint onto the websocket listener", () => {
  assert.equal(
    eventStreamUrl("http://127.0.0.1:8080"),
    `ws://127.0.0.1:8080${EVENT_STREAM_PATH}`,
  );
  assert.equal(
    eventStreamUrl("https://aion.example.com"),
    `wss://aion.example.com${EVENT_STREAM_PATH}`,
  );
  assert.equal(
    eventStreamUrl("http://aion.example.com:9000/"),
    `ws://aion.example.com:9000${EVENT_STREAM_PATH}`,
  );
  assert.equal(
    eventStreamUrl("https://aion.example.com/api/"),
    `wss://aion.example.com/api${EVENT_STREAM_PATH}`,
  );
});

test("eventStreamUrl rejects non-HTTP endpoints instead of inventing a scheme", () => {
  assert.throws(() => eventStreamUrl("ftp://example.com"), InvalidArgumentError);
  assert.throws(() => eventStreamUrl("not a url"), InvalidArgumentError);
});

test("subscription request frame carries the resume cursor when present", () => {
  const frame = JSON.parse(
    subscriptionRequestFrame({
      namespace: "tenant-a",
      workflowId: WORKFLOW_UUID,
      resumeFrom: 7,
    }),
  ) as {
    per_workflow: Record<string, unknown>;
  };
  assert.deepEqual(frame, {
    per_workflow: {
      namespace: "tenant-a",
      workflow_id: { uuid: WORKFLOW_UUID },
      resume_from_seq: 7,
    },
  });
});

test("subscription request frame omits resume_from_seq for a live tail", () => {
  const frame = JSON.parse(
    subscriptionRequestFrame({
      namespace: "tenant-a",
      workflowId: WORKFLOW_UUID,
    }),
  ) as {
    per_workflow: Record<string, unknown>;
  };
  assert.equal("resume_from_seq" in frame.per_workflow, false);
  assert.deepEqual(frame, {
    per_workflow: {
      namespace: "tenant-a",
      workflow_id: { uuid: WORKFLOW_UUID },
    },
  });
});

test("subscription request frame rejects invalid resume cursors: 0 and non-integers are never sent", () => {
  for (const resumeFrom of [0, -1, 1.5, Number.NaN]) {
    assert.throws(
      () =>
        subscriptionRequestFrame({
          namespace: "tenant-a",
          workflowId: WORKFLOW_UUID,
          resumeFrom,
        }),
      InvalidArgumentError,
      `resumeFrom=${resumeFrom} must be rejected`,
    );
  }
});

test("decodeStreamFrame maps terminal error frames through the wire-code table", () => {
  assert.throws(
    () => decodeStreamFrame(errorFrame("lagged", "subscriber lagged")),
    (error: unknown) => {
      assert.ok(error instanceof UnavailableError);
      assert.equal(error.message, "subscriber lagged");
      assert.equal(error.detail?.code, "lagged");
      return true;
    },
  );
  assert.throws(
    () => decodeStreamFrame(errorFrame("namespace_denied", "no grant")),
    NamespaceDeniedError,
  );
  assert.throws(
    () => decodeStreamFrame(errorFrame("not_found", "unknown workflow")),
    NotFoundError,
  );
  assert.throws(
    () =>
      decodeStreamFrame(
        errorFrame("invalid_input", "resume_from_seq must be >= 1"),
      ),
    InvalidArgumentError,
  );
  // Numeric wire-code spelling flows through the same table (7 = lagged).
  assert.throws(
    () =>
      decodeStreamFrame(JSON.stringify({ error: { code: 7, message: "lag" } })),
    UnavailableError,
  );
});

test("decodeStreamFrame passes StreamedEvent JSON through to the stream decoder", () => {
  const frame = decodeStreamFrame(JSON.stringify(streamedEvent(4)));
  const event = decodeWorkflowEvent(frame);
  assert.equal(event.seq, 4);
  assert.equal(event.namespace, "conformance");
});

test("decodeStreamFrame surfaces malformed frames as ServerError", () => {
  assert.throws(() => decodeStreamFrame("{not json"), ServerError);
});

test("integration: upgrade headers, subscription-first frame, resume cursor, and overlap dedupe over a real websocket", async () => {
  const connections: Array<{
    headers: IncomingMessage["headers"];
    frame: unknown;
  }> = [];

  await withWsServer(
    (socket, request) => {
      socket.once("message", (data) => {
        const frame = JSON.parse(String(data)) as {
          per_workflow: { resume_from_seq?: number };
        };
        connections.push({ headers: request.headers, frame });
        if (connections.length === 1) {
          socket.send(JSON.stringify(streamedEvent(1)));
          socket.send(JSON.stringify(streamedEvent(2)), () =>
            // Raw drop after both frames flushed: an abnormal close the
            // client must classify as transient.
            socket.terminate(),
          );
          return;
        }
        // The reconnect carries lastDelivered + 1 = 3; deliberately replay
        // an overlap (2, 3, 4) to prove the client dedupes replayed events.
        assert.equal(frame.per_workflow.resume_from_seq, 3);
        socket.send(JSON.stringify(streamedEvent(2)));
        socket.send(JSON.stringify(streamedEvent(3)));
        socket.send(JSON.stringify(streamedEvent(4)), () =>
          socket.close(1000, "stream end"),
        );
      });
    },
    async (endpoint) => {
      const transport = new WebSocketSubscribeTransport({
        endpoint,
        auth: {
          bearerToken: "token-1",
          subject: "alice",
          namespaces: ["tenant-a", "tenant-b"],
        },
      });

      const yielded: number[] = [];
      for await (const event of eventStream({
        transport,
        request: { namespace: "conformance", workflowId: WORKFLOW_UUID },
        maxReconnects: 3,
      })) {
        yielded.push(event.seq);
      }

      assert.deepEqual(yielded, [1, 2, 3, 4]);
      assert.equal(connections.length, 2);

      const upgrade = connections[0].headers;
      assert.equal(upgrade.authorization, "Bearer token-1");
      assert.equal(upgrade["x-aion-subject"], "alice");
      assert.equal(upgrade["x-aion-namespaces"], "tenant-a,tenant-b");

      assert.deepEqual(connections[0].frame, {
        per_workflow: {
          namespace: "conformance",
          workflow_id: { uuid: WORKFLOW_UUID },
        },
      });
      assert.deepEqual(connections[1].frame, {
        per_workflow: {
          namespace: "conformance",
          workflow_id: { uuid: WORKFLOW_UUID },
          resume_from_seq: 3,
        },
      });
    },
  );
});

test("integration: a lagged error frame reconnects with the cursor; delivery stays gap- and duplicate-free", async () => {
  let connections = 0;

  await withWsServer(
    (socket) => {
      socket.once("message", (data) => {
        connections += 1;
        const frame = JSON.parse(String(data)) as {
          per_workflow: { resume_from_seq?: number };
        };
        if (connections === 1) {
          assert.equal(frame.per_workflow.resume_from_seq, undefined);
          socket.send(JSON.stringify(streamedEvent(1)));
          socket.send(errorFrame("lagged", "subscriber lagged"));
          return;
        }
        assert.equal(frame.per_workflow.resume_from_seq, 2);
        socket.send(JSON.stringify(streamedEvent(2)), () =>
          socket.close(1000, "stream end"),
        );
      });
    },
    async (endpoint) => {
      const transport = new WebSocketSubscribeTransport({ endpoint });
      const yielded: number[] = [];
      for await (const event of eventStream({
        transport,
        request: { namespace: "conformance", workflowId: WORKFLOW_UUID },
        maxReconnects: 3,
      })) {
        yielded.push(event.seq);
      }
      assert.deepEqual(yielded, [1, 2]);
      assert.equal(connections, 2);
    },
  );
});

test("integration: a terminal error frame maps to the taxonomy and never reconnects", async () => {
  let connections = 0;

  await withWsServer(
    (socket) => {
      socket.once("message", () => {
        connections += 1;
        socket.send(
          errorFrame("namespace_denied", "subject is not granted tenant-b"),
          () => socket.close(1000),
        );
      });
    },
    async (endpoint) => {
      const transport = new WebSocketSubscribeTransport({ endpoint });
      const iterator = eventStream({
        transport,
        request: { namespace: "tenant-b", workflowId: WORKFLOW_UUID },
      })[Symbol.asyncIterator]();

      await assert.rejects(
        async () => iterator.next(),
        (error: unknown) => {
          assert.ok(error instanceof NamespaceDeniedError);
          assert.equal(error.message, "subject is not granted tenant-b");
          return true;
        },
      );
      assert.equal(connections, 1);
    },
  );
});

test("integration: breaking out of the stream closes the socket cleanly", async () => {
  const closed = withResolvers<number>();

  await withWsServer(
    (socket) => {
      socket.once("message", () => {
        socket.send(JSON.stringify(streamedEvent(1)));
        socket.send(JSON.stringify(streamedEvent(2)));
      });
      socket.on("close", (code) => closed.resolve(code));
    },
    async (endpoint) => {
      const transport = new WebSocketSubscribeTransport({ endpoint });
      for await (const event of eventStream({
        transport,
        request: { namespace: "conformance", workflowId: WORKFLOW_UUID },
      })) {
        assert.equal(event.seq, 1);
        break;
      }
      assert.equal(await closed.promise, 1000);
    },
  );
});

async function withWsServer(
  onConnection: (socket: ServerSocket, request: IncomingMessage) => void,
  run: (endpoint: string) => Promise<void>,
): Promise<void> {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise<void>((resolve, reject) => {
    server.once("listening", resolve);
    server.once("error", reject);
  });
  server.on("connection", onConnection);
  const address = server.address();
  if (address === null || typeof address === "string") {
    throw new Error("websocket test server did not bind a TCP port");
  }
  try {
    await run(`http://127.0.0.1:${address.port}`);
  } finally {
    for (const client of server.clients) {
      client.terminate();
    }
    await new Promise<void>((resolve, reject) => {
      server.close((error) => (error ? reject(error) : resolve()));
    });
  }
}

function withResolvers<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
} {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

/**
 * A StreamedEvent frame in the real server's wire shape: the aion-core
 * event (with its recording envelope holding the per-workflow seq) is
 * serde-encoded into the wire envelope's payload bytes.
 */
function streamedEvent(seq: number): unknown {
  const coreEvent = {
    type: "SignalReceived",
    data: {
      envelope: {
        seq,
        recorded_at: "2026-06-11T00:00:00Z",
        workflow_id: WORKFLOW_UUID,
      },
      name: "record",
      payload: {
        content_type: "Json",
        bytes: Array.from(new TextEncoder().encode(JSON.stringify({ seq }))),
      },
    },
  };
  return {
    namespace: "conformance",
    event: {
      namespace: "conformance",
      request_id: null,
      payload: {
        content_type: "application/json",
        bytes: Array.from(
          new TextEncoder().encode(JSON.stringify(coreEvent)),
        ),
      },
    },
  };
}

function errorFrame(code: string, message: string): string {
  return JSON.stringify({ error: { code, message } });
}
