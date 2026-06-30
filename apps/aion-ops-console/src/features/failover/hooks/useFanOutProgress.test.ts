import { describe, expect, test } from 'bun:test';
import { mergeEventsBySequence } from '@/features/workflow-detail/lib/timeline';
import { createAionEventWebSocketManager } from '@/lib/api';
import type {
  ManagedWebSocket,
  WebSocketClose,
  WebSocketEventHandler,
  WebSocketMessage,
} from '@/lib/api/websocket-types';
import type { Event } from '@/types';

import { tallyExactlyOnce } from '../lib/exactlyOnce';
import { deriveArity, deriveStatus } from './useFanOutProgress';

const WORKFLOW_ID = '00000000-0000-0000-0000-000000000001';
const NAMESPACE = 'demo';

function envelope(seq: number) {
  return { seq, recorded_at: '2026-06-29T00:00:00Z', workflow_id: WORKFLOW_ID };
}

function started(seq: number): Event {
  return {
    type: 'WorkflowStarted',
    data: {
      envelope: envelope(seq),
      workflow_type: 'collect_four',
      input: { content_type: 'Json', bytes: [123, 125] },
      run_id: '00000000-0000-0000-0000-0000000000a1',
      parent_run_id: null,
      package_version: '1.0.0',
    },
  } as Event;
}

function scheduled(seq: number, activityId: number): Event {
  return {
    type: 'ActivityScheduled',
    data: {
      envelope: envelope(seq),
      activity_id: activityId,
      activity_type: 'collect',
      input: { content_type: 'Json', bytes: [123, 125] },
    },
  } as Event;
}

function completed(seq: number, activityId: number): Event {
  return {
    type: 'ActivityCompleted',
    data: {
      envelope: envelope(seq),
      activity_id: activityId,
      result: { content_type: 'Json', bytes: [49] },
    },
  } as Event;
}

function workflowCompleted(seq: number): Event {
  return {
    type: 'WorkflowCompleted',
    data: {
      envelope: envelope(seq),
      result: { content_type: 'Json', bytes: [91, 93] },
    },
  } as Event;
}

/** Mirrors the hook's compose step: merge history+live by seq, then tally distinct. */
function progressOf(history: readonly Event[], live: readonly Event[]) {
  const events = mergeEventsBySequence(history, live);
  return {
    events,
    completedCount: tallyExactlyOnce(events).count,
    arity: deriveArity(events, null),
    status: deriveStatus(events),
  };
}

describe('firehose + describe merge', () => {
  test('history back-fill and live firehose frames merge by seq', () => {
    const historyBackfill = [started(1), scheduled(2, 0), completed(3, 0)];
    const liveFirehose = [scheduled(4, 1), completed(5, 1)];

    const progress = progressOf(historyBackfill, liveFirehose);

    expect(progress.events.map((e) => e.data.envelope.seq)).toEqual([1, 2, 3, 4, 5]);
    expect(progress.completedCount).toBe(2);
  });

  test('correct arity (distinct scheduled ordinals) and done status', () => {
    const events = [
      started(1),
      scheduled(2, 0),
      scheduled(3, 1),
      scheduled(4, 2),
      scheduled(5, 3),
      completed(6, 0),
      completed(7, 1),
      completed(8, 2),
      completed(9, 3),
      workflowCompleted(10),
    ];

    const progress = progressOf(events, []);

    expect(progress.arity).toBe(4);
    expect(progress.completedCount).toBe(4);
    expect(progress.status).toBe('Completed');
  });

  test('arity falls back to seed when no ActivityScheduled present yet', () => {
    expect(deriveArity([started(1)], 4)).toBe(4);
    expect(deriveArity([], 4)).toBe(4);
    expect(deriveArity([], null)).toBeNull();
  });

  test('status is Running until a terminal lifecycle event arrives', () => {
    expect(deriveStatus([started(1), completed(2, 0)])).toBe('Running');
    expect(deriveStatus([started(1), workflowCompleted(2)])).toBe('Completed');
  });
});

describe('reconnect produces NO double-count', () => {
  // The hook's resync path: firehose has no cursor, so on reconnect it drops the live
  // buffer, re-fetches describe (now containing everything), and re-merges by seq.

  test('describe back-fill overlapping live frames never shows 5/4', () => {
    // Pre-kill: history has 0/1, live firehose carried 2/3 completions.
    const historyBeforeKill = [
      started(1),
      scheduled(2, 0),
      scheduled(3, 1),
      scheduled(4, 2),
      scheduled(5, 3),
      completed(6, 0),
      completed(7, 1),
    ];
    const liveBeforeReconnect = [completed(8, 2), completed(9, 3)];

    const before = progressOf(historyBeforeKill, liveBeforeReconnect);
    expect(before.completedCount).toBe(4);

    // Reconnect: live buffer dropped; describe now returns the FULL history including
    // the two completions that had only arrived live. Re-merge must still be 4, not 5+.
    const describeAfterReconnect = [...historyBeforeKill, completed(8, 2), completed(9, 3)];
    const liveAfterReconnect: Event[] = [];

    const after = progressOf(describeAfterReconnect, liveAfterReconnect);
    expect(after.completedCount).toBe(4);
    expect(after.completedCount).not.toBe(5);
  });

  test('a duplicate live frame for an already-counted completion is ignored', () => {
    const history = [completed(6, 0), completed(7, 1)];
    const live = [completed(7, 1), completed(8, 2)];
    expect(progressOf(history, live).completedCount).toBe(3);
  });

  test('worst case: every event delivered twice still counts each once', () => {
    const all = [started(1), completed(6, 0), completed(7, 1), completed(8, 2), completed(9, 3)];
    expect(progressOf(all, all).completedCount).toBe(4);
  });
});

describe('firehose transport wiring (real manager)', () => {
  // Implements ManagedWebSocket so it satisfies WebSocketConstructor directly — the
  // handler signatures match the real socket contract, so no cast is needed.
  class CapturingSocket implements ManagedWebSocket {
    static instances: CapturingSocket[] = [];
    readonly sent: string[] = [];
    readyState = 0;
    onopen: WebSocketEventHandler<globalThis.Event> = null;
    onmessage: WebSocketEventHandler<WebSocketMessage> = null;
    onclose: WebSocketEventHandler<WebSocketClose> = null;
    onerror: WebSocketEventHandler<globalThis.Event> = null;

    constructor(readonly url: string) {
      CapturingSocket.instances.push(this);
    }

    send(data: string): void {
      this.sent.push(data);
    }
    close(): void {
      this.readyState = 3;
    }
    open(): void {
      this.readyState = 1;
      this.onopen?.(new Event('open'));
    }
    message(data: unknown): void {
      this.onmessage?.({ data });
    }
  }

  test('subscribe emits a firehose request and counts ActivityCompleted frames', () => {
    CapturingSocket.instances = [];
    const manager = createAionEventWebSocketManager({
      baseUrl: 'http://127.0.0.1:8090',
      webSocketImpl: CapturingSocket,
    });

    const received: Event[] = [];
    manager.subscribe({ kind: 'firehose', namespace: NAMESPACE }, (event) => received.push(event));

    const socket = CapturingSocket.instances[0];
    if (socket === undefined) {
      throw new Error('expected a socket to have been constructed');
    }
    socket.open();

    const subscribeMessage = JSON.parse(socket.sent[0] ?? '{}') as {
      subscription?: { firehose?: { namespace_selector?: string } };
    };
    expect(subscribeMessage.subscription?.firehose?.namespace_selector).toBe(NAMESPACE);

    socket.message(JSON.stringify({ namespace: NAMESPACE, event: completed(6, 0) }));
    socket.message(JSON.stringify({ namespace: NAMESPACE, event: completed(7, 1) }));
    // Duplicate live re-delivery of seq 7 — the tally must stay at 2.
    socket.message(JSON.stringify({ namespace: NAMESPACE, event: completed(7, 1) }));

    expect(tallyExactlyOnce(received).count).toBe(2);
    manager.reset();
  });
});
