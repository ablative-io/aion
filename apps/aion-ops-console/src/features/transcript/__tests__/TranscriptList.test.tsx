import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { ActivityEvent } from '@/types';

import {
  bodySizeLabel,
  isBodyClamped,
  TranscriptBody,
  TranscriptEntryRow,
} from '../components/TranscriptEventRow';
import { TranscriptList } from '../components/TranscriptList';
import type { TranscriptEntry } from '../hooks/useTranscript';

function event(kind: ActivityEvent['kind'], storeSeq: number | null): ActivityEvent {
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
    kind,
  };
}

function messageEntry(seq: number, text: string): TranscriptEntry {
  return {
    type: 'event',
    key: `seq:${seq}`,
    event: event({ kind: 'Message', role: { role: 'Assistant' }, text }, seq),
  };
}

describe('TranscriptList — bounded, non-virtualized scroller', () => {
  test('the scroll element is self-bounding and keeps its test id', () => {
    const markup = renderToStaticMarkup(<TranscriptList entries={[messageEntry(0, 'hello')]} />);
    // The bound lives directly on the scroller, so it never depends on an ancestor
    // resolving a definite height.
    expect(markup).toContain('data-testid="transcript-scroll"');
    expect(markup).toContain('max-h-[70vh]');
    expect(markup).toContain('overflow-y-auto');
    // The old percentage-height dependency is gone.
    expect(markup).not.toContain('h-full');
  });

  test('every entry renders — there is no windowing that could hide rows', () => {
    const entries = Array.from({ length: 40 }, (_, index) => messageEntry(index, `body-${index}`));
    const markup = renderToStaticMarkup(<TranscriptList entries={entries} />);
    // Every single row's payload is present — a windowing bug would drop the ones
    // outside a computed viewport (the original disappearing-content failure mode).
    for (let index = 0; index < entries.length; index += 1) {
      expect(markup).toContain(`body-${index}`);
    }
    // Stable per-entry keys are carried through to the DOM.
    expect(markup).toContain('data-entry-key="seq:0"');
    expect(markup).toContain('data-entry-key="seq:39"');
  });

  test('the first commit follows the tail: no "Jump to latest", nothing animates', () => {
    const markup = renderToStaticMarkup(
      <TranscriptList entries={[messageEntry(0, 'a'), messageEntry(1, 'b')]} />
    );
    // `following` defaults to true, so the jump affordance is hidden.
    expect(markup).not.toContain('Jump to latest');
    // The initial-commit guard (seenKeysRef starts null) means the very first render
    // never treats an entry as "new", so no entrance animation classes are emitted.
    expect(markup).not.toContain('animate-in');
    expect(markup).not.toContain('slide-in-from-bottom');
  });
});

describe('body clamp — oversized payloads collapse without dropping content', () => {
  const shortBody = 'a small tool result';
  const longBody = 'line\n'.repeat(1000); // 5000 chars, 1001 lines

  test('isBodyClamped triggers only past the 4000-char threshold', () => {
    expect(isBodyClamped(shortBody)).toBe(false);
    expect(isBodyClamped('x'.repeat(4000))).toBe(false);
    expect(isBodyClamped('x'.repeat(4001))).toBe(true);
    expect(isBodyClamped(longBody)).toBe(true);
  });

  test('bodySizeLabel reports the honest line count and byte size', () => {
    expect(bodySizeLabel(longBody)).toBe('1001 lines · 4.9 KB');
    expect(bodySizeLabel('one line')).toBe('1 line · 0.0 KB');
  });

  test('a short body renders as a plain pre with no toggle', () => {
    const markup = renderToStaticMarkup(
      <TranscriptBody body={shortBody} expanded={false} onToggle={() => {}} />
    );
    expect(markup).toContain(shortBody);
    expect(markup).not.toContain('Show all');
    expect(markup).not.toContain('Collapse');
    expect(markup).not.toContain('max-h-64');
  });

  test('a long body collapsed shows the full text (clipped by CSS) behind a "Show all" toggle', () => {
    const markup = renderToStaticMarkup(
      <TranscriptBody body={longBody} expanded={false} onToggle={() => {}} />
    );
    // Nothing is dropped — the whole payload is in the DOM, only visually clamped.
    expect(markup).toContain(longBody);
    expect(markup).toContain('max-h-64');
    expect(markup).toContain('overflow-hidden');
    expect(markup).toContain('Show all (1001 lines · 4.9 KB)');
    expect(markup).not.toContain('>Collapse<');
  });

  test('a long body expanded shows everything with a "Collapse" toggle and no clamp', () => {
    const markup = renderToStaticMarkup(
      <TranscriptBody body={longBody} expanded={true} onToggle={() => {}} />
    );
    expect(markup).toContain(longBody);
    expect(markup).toContain('Collapse');
    expect(markup).not.toContain('Show all');
    expect(markup).not.toContain('max-h-64');
  });

  test('the row wires an oversized event body straight into the clamp affordance', () => {
    const entry: TranscriptEntry = {
      type: 'event',
      key: 'seq:9',
      event: event({ kind: 'Message', role: { role: 'Assistant' }, text: longBody }, 9),
    };
    const markup = renderToStaticMarkup(<TranscriptEntryRow entry={entry} />);
    expect(markup).toContain('Show all');
    expect(markup).toContain('data-entry-key="seq:9"');
  });
});
