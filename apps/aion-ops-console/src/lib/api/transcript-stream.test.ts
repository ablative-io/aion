import { expect, test } from 'bun:test';

import type { ActivityEvent } from '@/types';

import { createAionTranscriptStreamManager, type TranscriptTarget } from './transcript-stream';

const target: TranscriptTarget = {
  namespace: 'default',
  workflowId: '00000000-0000-0000-0000-000000000001',
  activityId: 3,
  attempt: 1,
};

function activityEvent(storeSeq: number | null, ephemeral: boolean): ActivityEvent {
  return {
    workflow_id: target.workflowId,
    activity_id: target.activityId,
    attempt: target.attempt,
    agent_id: '00000000-0000-0000-0000-0000000000aa',
    agent_role: 'orchestrator',
    emitted_at: '2026-07-01T00:00:00Z',
    worker_seq: 1,
    store_seq: storeSeq,
    ephemeral,
    kind: ephemeral
      ? { kind: 'Delta', message_id: 'm1', text_fragment: 'wor' }
      : { kind: 'Message', role: { role: 'Assistant' }, text: 'hello' },
  };
}

function frame(event: ActivityEvent): string {
  return JSON.stringify({ kind: 'activity_event', event });
}

test('subscribe sends a transcript frame WITHOUT after_seq for a fresh subscriber', () => {
  const factory = new FakeSocketFactory();
  const manager = createAionTranscriptStreamManager({
    baseUrl: 'https://aion.example',
    target,
    webSocketImpl: factory.ctor,
  });

  manager.subscribe({ onEvent: () => {} });
  const socket = factory.sockets[0] as FakeSocket;
  socket.open();

  expect(socket.url).toBe('wss://aion.example/events/stream');
  expect(JSON.parse(socket.sent[0] ?? '{}')).toEqual({
    type: 'subscribe',
    subscription: {
      transcript: {
        namespace: target.namespace,
        workflow_id: target.workflowId,
        activity_id: target.activityId,
        attempt: target.attempt,
      },
    },
  });
});

test('reconnect resumes from the highest applied store_seq (no gap/dup)', () => {
  const factory = new FakeSocketFactory();
  const scheduler = new ImmediateScheduler();
  const manager = createAionTranscriptStreamManager({
    target,
    webSocketImpl: factory.ctor,
    scheduler,
  });
  const received: ActivityEvent[] = [];

  manager.subscribe({ onEvent: (event) => received.push(event) });
  const first = factory.sockets[0] as FakeSocket;
  first.open();
  // Durable tail: store_seq 0 and 1 arrive; an ephemeral delta does NOT advance.
  first.message(frame(activityEvent(0, false)));
  first.message(frame(activityEvent(1, false)));
  first.message(frame(activityEvent(null, true)));

  // The socket drops; the manager reconnects and must resume after store_seq 1.
  first.drop();
  scheduler.flush();
  const second = factory.sockets[1] as FakeSocket;
  second.open();

  const resume = JSON.parse(second.sent[0] ?? '{}') as {
    subscription: { transcript: { after_seq?: number } };
  };
  expect(resume.subscription.transcript.after_seq).toBe(1);
  expect(received.map((event) => event.store_seq)).toEqual([0, 1, null]);
});

test('a transcript_lagged terminal is surfaced as a typed error and keeps the cursor', () => {
  const factory = new FakeSocketFactory();
  const scheduler = new ImmediateScheduler();
  const manager = createAionTranscriptStreamManager({
    target,
    webSocketImpl: factory.ctor,
    scheduler,
  });
  const errors: (string | null)[] = [];
  manager.onError((error) => errors.push(error?.message ?? null));

  manager.subscribe({ onEvent: () => {} });
  const socket = factory.sockets[0] as FakeSocket;
  socket.open();
  socket.message(frame(activityEvent(4, false)));
  socket.message(JSON.stringify({ error: { code: 'transcript_lagged', skipped: 9 } }));

  expect(errors.some((message) => message?.includes('9 live update'))).toBe(true);

  // After a reconnect the cursor is KEPT (the durable tail is authoritative): the
  // resume request is after the last applied store_seq (4), not from scratch.
  socket.drop();
  scheduler.flush();
  const second = factory.sockets[1] as FakeSocket;
  second.open();
  const resume = JSON.parse(second.sent[0] ?? '{}') as {
    subscription: { transcript: { after_seq?: number } };
  };
  expect(resume.subscription.transcript.after_seq).toBe(4);
});

test('an unrecognized frame is surfaced as a decode error, never silently dropped', () => {
  const factory = new FakeSocketFactory();
  const manager = createAionTranscriptStreamManager({
    target,
    webSocketImpl: factory.ctor,
    warn: () => {},
  });
  const errors: (string | null)[] = [];
  manager.onError((error) => errors.push(error?.kind ?? null));

  manager.subscribe({ onEvent: () => {} });
  const socket = factory.sockets[0] as FakeSocket;
  socket.open();
  socket.message(JSON.stringify({ kind: 'something_else', event: {} }));

  expect(errors).toContain('frame-decode');
});

class ImmediateScheduler {
  private pending: (() => void)[] = [];

  setTimeout(callback: () => void): ReturnType<typeof setTimeout> {
    this.pending.push(callback);
    return this.pending.length as unknown as ReturnType<typeof setTimeout>;
  }

  clearTimeout(): void {}

  flush(): void {
    const callbacks = this.pending;
    this.pending = [];
    for (const callback of callbacks) {
      callback();
    }
  }
}

class FakeSocketFactory {
  readonly sockets: FakeSocket[] = [];
  readonly ctor: new (
    url: string
  ) => FakeSocket;

  constructor() {
    const sockets = this.sockets;
    this.ctor = class extends FakeSocket {
      constructor(url: string) {
        super(url);
        sockets.push(this);
      }
    };
  }
}

class FakeSocket {
  readonly sent: string[] = [];
  readyState = 0;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: ((event: { wasClean: boolean }) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;

  constructor(readonly url: string) {}

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = 3;
    this.onclose?.({ wasClean: true });
  }

  open(): void {
    this.readyState = 1;
    this.onopen?.({} as Event);
  }

  message(data: unknown): void {
    this.onmessage?.({ data });
  }

  drop(): void {
    this.readyState = 3;
    this.onclose?.({ wasClean: false });
  }
}
