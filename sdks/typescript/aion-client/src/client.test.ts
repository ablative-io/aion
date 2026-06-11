import assert from "node:assert/strict";
import test from "node:test";
import { connect } from "./client.js";
import {
  InvalidArgumentError,
  NamespaceDeniedError,
  QueryFailedError,
  QueryTimeoutError,
  ServerError,
  UnauthenticatedError,
  mapHttpResponseError,
  mapTransportError,
  mapWireError,
} from "./errors.js";
import { fromPayload, toPayload } from "./payload.js";
import { type SubscribeRequest, eventStream } from "./stream.js";

test("maps branchable server and transport errors", () => {
  assert.equal(
    mapWireError({ code: "not_found", message: "missing" }).kind,
    "NotFound",
  );
  assert.equal(
    mapWireError({ code: "query_timeout", message: "timeout" }).kind,
    "QueryTimeout",
  );
  assert.equal(
    mapTransportError(new TypeError("connection refused")).kind,
    "Unavailable",
  );
});

test("maps sequence_conflict to ServerError: it signals a server double-writer bug, not idempotency", () => {
  for (const code of ["sequence_conflict", "WIRE_ERROR_CODE_SEQUENCE_CONFLICT", 3]) {
    const error = mapWireError({ code, message: "conflict" });
    assert.ok(
      error instanceof ServerError,
      `wire code ${JSON.stringify(code)} must map to ServerError`,
    );
    assert.equal(error.kind, "Server");
    assert.equal(error.detail?.code, "sequence_conflict");
  }
});

test("maps invalid_input wire code spellings to InvalidArgumentError", () => {
  for (const code of ["invalid_input", "WIRE_ERROR_CODE_INVALID_INPUT", 8]) {
    const error = mapWireError({ code, message: "bad input" });
    assert.ok(
      error instanceof InvalidArgumentError,
      `wire code ${JSON.stringify(code)} must map to InvalidArgumentError`,
    );
    assert.equal(error.kind, "InvalidArgument");
    assert.equal(error.detail?.code, "invalid_input");
  }
});

test("maps query_failed wire code spellings to QueryFailedError", () => {
  // Numeric pin: query_failed is wire enum value 10.
  for (const code of ["query_failed", "WIRE_ERROR_CODE_QUERY_FAILED", 10]) {
    const error = mapWireError({ code, message: "handler raised" });
    assert.ok(
      error instanceof QueryFailedError,
      `wire code ${JSON.stringify(code)} must map to QueryFailedError`,
    );
    assert.equal(error.kind, "QueryFailed");
    assert.equal(error.detail?.code, "query_failed");
  }
});

test("maps backend wire code spellings to ServerError", () => {
  for (const code of ["backend", "WIRE_ERROR_CODE_BACKEND", 9]) {
    const error = mapWireError({ code, message: "store failure" });
    assert.ok(
      error instanceof ServerError,
      `wire code ${JSON.stringify(code)} must map to ServerError`,
    );
    assert.equal(error.kind, "Server");
    assert.equal(error.detail?.code, "backend");
  }
});

test("HTTP 409 defers to the body wire code: sequence_conflict is ServerError, idempotency_conflict stays AlreadyExists", async () => {
  const sequenceConflict = await mapHttpResponseError(
    new Response(
      JSON.stringify({ code: "sequence_conflict", message: "double writer" }),
      { status: 409, statusText: "Conflict" },
    ),
  );
  assert.ok(sequenceConflict instanceof ServerError);
  assert.equal(sequenceConflict.kind, "Server");
  assert.equal(sequenceConflict.detail?.code, "sequence_conflict");
  assert.equal(sequenceConflict.detail?.status, 409);

  const idempotencyConflict = await mapHttpResponseError(
    new Response(
      JSON.stringify({
        code: "idempotency_conflict",
        message: "conflicting reuse",
      }),
      { status: 409, statusText: "Conflict" },
    ),
  );
  assert.equal(idempotencyConflict.kind, "AlreadyExists");
  assert.equal(idempotencyConflict.detail?.status, 409);
});

test("maps HTTP 403 with a namespace_denied body to NamespaceDeniedError", async () => {
  const response = new Response(
    JSON.stringify({
      code: "namespace_denied",
      message: "subject is not granted namespace tenant-a",
    }),
    { status: 403, statusText: "Forbidden" },
  );

  const error = await mapHttpResponseError(response);

  assert.ok(error instanceof NamespaceDeniedError);
  assert.equal(error.kind, "NamespaceDenied");
  assert.equal(error.message, "subject is not granted namespace tenant-a");
  assert.equal(error.detail?.status, 403);
});

test("maps HTTP 401 to UnauthenticatedError, distinct from namespace denial", async () => {
  const response = new Response(
    JSON.stringify({ code: "unauthenticated", message: "bad token" }),
    { status: 401, statusText: "Unauthorized" },
  );

  const error = await mapHttpResponseError(response);

  assert.ok(error instanceof UnauthenticatedError);
  assert.equal(error.kind, "Unauthenticated");
  assert.equal(error.message, "bad token");
  assert.equal(error.detail?.status, 401);
});

test("maps every namespace_denied wire code spelling to NamespaceDeniedError", () => {
  for (const code of ["namespace_denied", "WIRE_ERROR_CODE_NAMESPACE_DENIED", 2]) {
    const error = mapWireError({ code, message: "namespace denied" });
    assert.ok(
      error instanceof NamespaceDeniedError,
      `wire code ${JSON.stringify(code)} must map to NamespaceDeniedError`,
    );
    assert.equal(error.kind, "NamespaceDenied");
    assert.equal(error.message, "namespace denied");
    assert.equal(error.detail?.code, "namespace_denied");
  }
});

test("maps the unauthenticated wire code to UnauthenticatedError", () => {
  const error = mapWireError({
    code: "unauthenticated",
    message: "credential rejected",
  });

  assert.ok(error instanceof UnauthenticatedError);
  assert.equal(error.kind, "Unauthenticated");
  assert.equal(error.message, "credential rejected");
  assert.equal(error.detail?.code, "unauthenticated");
});

test("namespace denial on a stream is terminal and never retried as transient", async () => {
  let subscribes = 0;
  const transport = {
    async *subscribe(): AsyncIterable<unknown> {
      subscribes += 1;
      yield {
        error: {
          code: "namespace_denied",
          message: "subject is not granted namespace tenant-a",
        },
      };
    },
  };

  const iterator = eventStream({
    transport,
    request: { namespace: "tenant-a", workflowId: "workflow" },
  })[Symbol.asyncIterator]();

  await assert.rejects(
    async () => iterator.next(),
    (error: unknown) => {
      assert.ok(error instanceof NamespaceDeniedError);
      assert.equal(error.kind, "NamespaceDenied");
      assert.equal(error.detail?.code, "namespace_denied");
      assert.equal(
        error.message,
        "subject is not granted namespace tenant-a",
      );
      return true;
    },
  );
  assert.equal(subscribes, 1);
});

test("a wrapped lagged error frame is classified Unavailable and the stream reconnects", async () => {
  assert.equal(
    mapWireError({ code: "lagged", message: "subscriber lagged" }).kind,
    "Unavailable",
  );

  let subscribes = 0;
  const resumeRequests: Array<number | undefined> = [];
  const transport = {
    subscribe(request: SubscribeRequest): AsyncIterable<unknown> {
      subscribes += 1;
      resumeRequests.push(request.resumeFrom);
      return subscribes === 1 ? laggedFrames() : resumedFrames();
    },
  };
  async function* laggedFrames(): AsyncIterable<unknown> {
    yield frame(1);
    yield { error: { code: "lagged", message: "subscriber lagged" } };
  }
  async function* resumedFrames(): AsyncIterable<unknown> {
    yield frame(2);
  }

  const yielded: number[] = [];
  for await (const event of eventStream({
    transport,
    request: { namespace: "default", workflowId: "workflow" },
  })) {
    yielded.push(event.seq);
  }

  assert.deepEqual(yielded, [1, 2]);
  assert.equal(subscribes, 2);
  assert.deepEqual(resumeRequests, [undefined, 2]);
});

test("round-trips typed payloads through JSON payload helpers", () => {
  const input = { name: "demo", count: 3, nested: { ok: true } };
  assert.deepEqual(fromPayload<typeof input>(toPayload(input)), input);
});

test("resuming event stream yields every event once after a transient drop", async () => {
  const transport = new StubSubscribeTransport();
  const yielded: number[] = [];

  for await (const event of eventStream({
    transport,
    request: { namespace: "default", workflowId: "workflow" },
    maxReconnects: 2,
  })) {
    yielded.push(event.seq);
  }

  assert.deepEqual(yielded, [1, 2, 3]);
  assert.deepEqual(transport.resumeRequests, [undefined, 3]);
});

test("resumed stream drops server replay overlap: every event is delivered exactly once", async () => {
  let subscribes = 0;
  const resumeRequests: Array<number | undefined> = [];
  const transport = {
    subscribe(request: SubscribeRequest): AsyncIterable<unknown> {
      subscribes += 1;
      resumeRequests.push(request.resumeFrom);
      return subscribes === 1 ? initialFrames() : overlappingReplay();
    },
  };
  async function* initialFrames(): AsyncIterable<unknown> {
    yield frame(1);
    yield frame(2);
    throw new TypeError("socket closed");
  }
  // A server replay may overlap the cursor (subscribe-then-snapshot); the
  // stream machinery must drop seqs <= lastDelivered, never re-yield them.
  async function* overlappingReplay(): AsyncIterable<unknown> {
    yield frame(2);
    yield frame(3);
    yield frame(4);
  }

  const yielded: number[] = [];
  for await (const event of eventStream({
    transport,
    request: { namespace: "default", workflowId: "workflow" },
    maxReconnects: 1,
  })) {
    yielded.push(event.seq);
  }

  assert.deepEqual(yielded, [1, 2, 3, 4]);
  assert.deepEqual(resumeRequests, [undefined, 3]);
});

test("terminal stream failures throw out of the async iterator", async () => {
  const transport = {
    async *subscribe(): AsyncIterable<unknown> {
      yield frame(1);
      yield {
        error: { code: "query_failed", message: "workflow terminal failure" },
      };
    },
  };

  const iterator = eventStream({
    transport,
    request: { namespace: "default", workflowId: "workflow" },
  })[Symbol.asyncIterator]();

  assert.equal((await iterator.next()).value.seq, 1);
  await assert.rejects(async () => iterator.next(), QueryFailedError);
});

test("invalid stream frames fail terminally instead of reconnecting forever", async () => {
  const transport = {
    async *subscribe(): AsyncIterable<unknown> {
      yield { event: { namespace: "default" } };
    },
  };

  const iterator = eventStream({
    transport,
    request: { namespace: "default", workflowId: "workflow" },
    maxReconnects: 1,
  })[Symbol.asyncIterator]();

  await assert.rejects(async () => iterator.next(), ServerError);
});

test("query is bounded by the caller deadline and surfaces QueryTimeoutError", async () => {
  const transport = {
    start: () => Promise.reject(new Error("unused")),
    signal: () => Promise.reject(new Error("unused")),
    cancel: () => Promise.reject(new Error("unused")),
    list: () => Promise.reject(new Error("unused")),
    describe: () => Promise.reject(new Error("unused")),
    // A query that never resolves: only the caller deadline can end it.
    query: () => new Promise<never>(() => undefined),
  };
  const client = await connect({
    endpoint: "http://127.0.0.1:1",
    transport,
    streamTransport: new StubSubscribeTransport(),
  });

  await assert.rejects(
    client.query({ workflowId: "wf", queryName: "state", timeoutMs: 20 }),
    (error: unknown) => {
      assert.ok(error instanceof QueryTimeoutError, String(error));
      assert.match(error.message, /20ms/);
      return true;
    },
  );
});

test("non-positive query deadlines are InvalidArgumentError before any transport call", async () => {
  let called = 0;
  const transport = {
    start: () => Promise.reject(new Error("unused")),
    signal: () => Promise.reject(new Error("unused")),
    cancel: () => Promise.reject(new Error("unused")),
    list: () => Promise.reject(new Error("unused")),
    describe: () => Promise.reject(new Error("unused")),
    query: () => {
      called += 1;
      return Promise.reject(new Error("must not be called"));
    },
  };
  const client = await connect({
    endpoint: "http://127.0.0.1:1",
    transport,
    streamTransport: new StubSubscribeTransport(),
  });

  for (const timeoutMs of [0, -5, Number.NaN]) {
    await assert.rejects(
      client.query({ workflowId: "wf", queryName: "state", timeoutMs }),
      (error: unknown) => error instanceof InvalidArgumentError,
    );
  }
  assert.equal(called, 0);
});

class StubSubscribeTransport {
  readonly resumeRequests: Array<number | undefined> = [];

  subscribe(request: SubscribeRequest): AsyncIterable<unknown> {
    this.resumeRequests.push(request.resumeFrom);
    return this.frames(request.resumeFrom);
  }

  private async *frames(
    resumeFrom: number | undefined,
  ): AsyncIterable<unknown> {
    if (resumeFrom === undefined) {
      yield frame(1);
      yield frame(2);
      throw new TypeError("socket closed");
    }
    yield frame(resumeFrom);
  }
}

function frame(seq: number): unknown {
  return {
    namespace: "default",
    event: {
      namespace: "default",
      seq,
      payload: toPayload({ seq }),
    },
  };
}
