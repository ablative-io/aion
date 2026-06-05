import assert from "node:assert/strict";
import test from "node:test";
import {
  QueryFailedError,
  ServerError,
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
    mapWireError({ code: "sequence_conflict", message: "conflict" }).kind,
    "AlreadyExists",
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
