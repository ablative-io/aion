import { describe, expect, test } from 'bun:test';

import type { ActivityEvent } from '@/types';

import { foldTranscriptEvent, type TranscriptEntry } from '../hooks/useTranscript';

function event(storeSeq: number | null): ActivityEvent {
  return {
    workflow_id: '00000000-0000-0000-0000-000000000001',
    activity_id: 3,
    attempt: 1,
    agent_id: '00000000-0000-0000-0000-0000000000aa',
    agent_role: 'orchestrator',
    emitted_at: '2026-07-01T00:00:00Z',
    worker_seq: 1,
    store_seq: storeSeq,
    ephemeral: storeSeq === null,
    kind:
      storeSeq === null
        ? { kind: 'Delta', message_id: 'm', text_fragment: 'x' }
        : { kind: 'Message', role: { role: 'Assistant' }, text: `seq-${storeSeq}` },
  };
}

function seqs(entries: TranscriptEntry[]): (number | null)[] {
  return entries.map((entry) => entry.event.store_seq);
}

describe('foldTranscriptEvent', () => {
  test('orders persisted events by store_seq regardless of arrival order', () => {
    let entries: TranscriptEntry[] = [];
    entries = foldTranscriptEvent(entries, event(2), 0);
    entries = foldTranscriptEvent(entries, event(0), 1);
    entries = foldTranscriptEvent(entries, event(1), 2);
    expect(seqs(entries)).toEqual([0, 1, 2]);
  });

  test('deduplicates a persisted event re-delivered on a reconnect race', () => {
    let entries: TranscriptEntry[] = [];
    entries = foldTranscriptEvent(entries, event(5), 0);
    const before = entries;
    entries = foldTranscriptEvent(entries, event(5), 1);
    // Same store_seq => same event: the duplicate is dropped and the array
    // reference is unchanged so React can skip the re-render.
    expect(entries).toBe(before);
    expect(seqs(entries)).toEqual([5]);
  });

  test('appends ephemeral deltas in arrival order with unique keys, keeping them at the tail', () => {
    let entries: TranscriptEntry[] = [];
    entries = foldTranscriptEvent(entries, event(0), 0);
    entries = foldTranscriptEvent(entries, event(null), 1);
    entries = foldTranscriptEvent(entries, event(null), 2);
    expect(seqs(entries)).toEqual([0, null, null]);
    // Each ephemeral delta has a distinct React key (a monotonic delta id).
    expect(new Set(entries.map((entry) => entry.key)).size).toBe(3);
  });

  test('two ephemeral deltas are never treated as duplicates even with equal payloads', () => {
    let entries: TranscriptEntry[] = [];
    entries = foldTranscriptEvent(entries, event(null), 10);
    entries = foldTranscriptEvent(entries, event(null), 11);
    expect(entries).toHaveLength(2);
  });
});
