import { describe, expect, test } from 'bun:test';

import type { ActivityEvent } from '@/types';

import { foldTranscriptEvent, type TranscriptEntry } from '../hooks/useTranscript';

const AGENT_A = '00000000-0000-0000-0000-0000000000aa';
const AGENT_B = '00000000-0000-0000-0000-0000000000bb';

function event(
  storeSeq: number | null,
  kind: ActivityEvent['kind'],
  agentId: string = AGENT_A
): ActivityEvent {
  return {
    workflow_id: '00000000-0000-0000-0000-000000000001',
    activity_id: 3,
    attempt: 1,
    agent_id: agentId,
    agent_role: 'root',
    emitted_at: '2026-07-01T00:00:00Z',
    worker_seq: 1,
    store_seq: storeSeq,
    ephemeral: storeSeq === null,
    kind,
  };
}

function message(storeSeq: number, text = `seq-${storeSeq}`, agentId?: string): ActivityEvent {
  return event(storeSeq, { kind: 'Message', role: { role: 'Assistant' }, text }, agentId);
}

function delta(messageId: string, fragment: string, agentId?: string): ActivityEvent {
  return event(null, { kind: 'Delta', message_id: messageId, text_fragment: fragment }, agentId);
}

function note(storeSeq: number, text: string, agentId?: string): ActivityEvent {
  return event(storeSeq, { kind: 'Progress', detail: { detail: 'Note', text } }, agentId);
}

function reasoningRaw(storeSeq: number, itemId: string, summary: string): ActivityEvent {
  return event(storeSeq, {
    kind: 'Raw',
    source: 'event/item',
    value: {
      type: 'reasoning_item',
      item: {
        id: itemId,
        encrypted_content: 'gAAAA-opaque',
        content: [],
        summary: [{ type: 'summary_text', text: summary }],
      },
    },
  });
}

function fold(entries: TranscriptEntry[], events: ActivityEvent[], firstId = 0): TranscriptEntry[] {
  let next = entries;
  let deltaId = firstId;
  for (const incoming of events) {
    next = foldTranscriptEvent(next, incoming, deltaId);
    deltaId += 1;
  }
  return next;
}

function seqs(entries: TranscriptEntry[]): (number | null)[] {
  return entries.map((entry) => entry.event.store_seq);
}

describe('foldTranscriptEvent', () => {
  test('orders persisted events by store_seq regardless of arrival order', () => {
    const entries = fold([], [message(2), message(0), message(1)]);
    expect(seqs(entries)).toEqual([0, 1, 2]);
  });

  test('deduplicates a persisted event re-delivered on a reconnect race', () => {
    let entries = fold([], [message(5)]);
    const before = entries;
    entries = foldTranscriptEvent(entries, message(5), 1);
    // Same store_seq => same event: the duplicate is dropped and the array
    // reference is unchanged so React can skip the re-render.
    expect(entries).toBe(before);
    expect(seqs(entries)).toEqual([5]);
  });

  test('coalesces deltas for one message into a single entry whose text grows in place', () => {
    const entries = fold([], [delta('msg_1', ' the'), delta('msg_1', ' diagnostics')]);
    expect(entries).toHaveLength(1);
    const first = entries[0];
    if (first?.type !== 'stream') {
      throw new Error(`expected a stream entry, got ${String(first?.type)}`);
    }
    expect(first.text).toBe(' the diagnostics');

    // The key stays stable while the stream grows (React updates the same row).
    const grown = fold(entries, [delta('msg_1', ' crate')]);
    expect(grown).toHaveLength(1);
    expect(grown[0]?.key).toBe(first.key);
    expect(grown[0]?.type === 'stream' ? grown[0].text : '').toBe(' the diagnostics crate');
  });

  test('deltas for different messages or agents stay separate streams', () => {
    const entries = fold(
      [],
      [delta('msg_1', 'a'), delta('msg_2', 'b'), delta('msg_1', 'c', AGENT_B)]
    );
    expect(entries).toHaveLength(3);
    expect(new Set(entries.map((entry) => entry.key)).size).toBe(3);
  });

  test('a complete Message replaces its own delta stream — never rendered twice', () => {
    let entries = fold([], [delta('msg_1', 'hel'), delta('msg_1', 'lo')]);
    entries = foldTranscriptEvent(entries, message(4, 'hello'), 2);
    expect(entries).toHaveLength(1);
    expect(entries[0]?.type).toBe('event');
    expect(entries[0]?.event.kind).toEqual({
      kind: 'Message',
      role: { role: 'Assistant' },
      text: 'hello',
    });
  });

  test("a Message finalizes only the producing agent's streams", () => {
    let entries = fold([], [delta('msg_1', 'mine'), delta('msg_9', 'other', AGENT_B)]);
    entries = foldTranscriptEvent(entries, message(4, 'mine'), 2);
    expect(entries).toHaveLength(2);
    const remaining = entries.find((entry) => entry.type === 'stream');
    expect(remaining?.type === 'stream' ? remaining.text : '').toBe('other');
  });

  test('a reasoning_item raw finalizes the thinking stream with the same item id', () => {
    let entries = fold([], [delta('rs_1', 'thinking…'), delta('msg_1', 'answer…')]);
    entries = foldTranscriptEvent(entries, reasoningRaw(2, 'rs_1', 'Planning'), 2);
    // The rs_1 stream is replaced by the persisted reasoning item; the unrelated
    // msg_1 stream keeps streaming.
    expect(entries).toHaveLength(2);
    expect(entries.filter((entry) => entry.type === 'stream')).toHaveLength(1);
    expect(entries.filter((entry) => entry.type === 'event')).toHaveLength(1);
  });

  test('coalesces consecutive identical progress notes into one counted run', () => {
    const entries = fold(
      [],
      [note(1, 'tool_call_delta'), note(2, 'tool_call_delta'), note(3, 'tool_call_delta')]
    );
    expect(entries).toHaveLength(1);
    const run = entries[0];
    if (run?.type !== 'notes') {
      throw new Error(`expected a notes run, got ${String(run?.type)}`);
    }
    expect(run.count).toBe(3);
    expect(run.seqs).toEqual([1, 2, 3]);
  });

  test('a replayed note store_seq inside a run is dropped as a duplicate', () => {
    const entries = fold([], [note(1, 'tool_call_delta'), note(2, 'tool_call_delta')]);
    const replayed = foldTranscriptEvent(entries, note(2, 'tool_call_delta'), 9);
    expect(replayed).toBe(entries);
  });

  test('a different note text or agent starts a new run', () => {
    const entries = fold(
      [],
      [note(1, 'tool_call_delta'), note(2, 'compacting'), note(3, 'compacting', AGENT_B)]
    );
    expect(entries).toHaveLength(3);
  });

  test('a usage-estimate progress event is never absorbed into a note run', () => {
    const usage: ActivityEvent = event(2, {
      kind: 'Progress',
      detail: { detail: 'UsageEstimate', input_tokens: 10, output_tokens: 5 },
    });
    const entries = fold([], [note(1, 'tool_call_delta')]);
    const next = foldTranscriptEvent(entries, usage, 1);
    expect(next).toHaveLength(2);
    expect(next[1]?.type).toBe('event');
  });
});
