import { describe, expect, it } from "vitest";
import type { WorkerConfig, WorkerSession, WorkerSessionEvent } from "./index.js";

class FakeSession implements WorkerSession {
  public handshakes: WorkerConfig[] = [];
  public registrations: readonly string[][] = [];

  public async handshake(config: WorkerConfig): Promise<void> {
    this.handshakes.push(config);
  }

  public async register(activityTypes: readonly string[]): Promise<void> {
    this.registrations.push([...activityTypes]);
  }

  public async *receiveTasks(): AsyncIterable<WorkerSessionEvent> {
    yield { kind: "closed" };
  }

  public async reportResult(): Promise<void> {}

  public async reportFailure(): Promise<void> {}

  public async sendHeartbeat(): Promise<void> {}

  public async close(): Promise<void> {}
}

describe("WorkerSession", () => {
  it("carries configured task queue, identity, and activity names", async () => {
    const session = new FakeSession();
    const config: WorkerConfig = {
      endpoint: "127.0.0.1:50051",
      taskQueue: "payments",
      identity: "worker-a",
      maxConcurrency: 2,
    };

    await session.handshake(config);
    await session.register(["charge", "refund"]);

    expect(session.handshakes).toHaveLength(1);
    expect(session.handshakes[0]?.taskQueue).toBe("payments");
    expect(session.handshakes[0]?.identity).toBe("worker-a");
    expect(session.registrations).toEqual([["charge", "refund"]]);
  });
});
