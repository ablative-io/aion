import assert from "node:assert/strict";
import test from "node:test";
import { mapTransportError, mapWireError } from "./errors.js";
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
