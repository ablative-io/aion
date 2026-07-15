import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { AttemptCapabilities } from '@/lib/api';
import type { ActivityEvent } from '@/types';
import { TranscriptEntryRow, TranscriptEventRow } from '../components/TranscriptEventRow';
import { TranscriptPanelContent } from '../components/TranscriptPanel';
import { sortAttempts } from '../hooks/useActivityAttempts';
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

describe('TranscriptPanelContent', () => {
  test('with no target it prompts to select an attempt', () => {
    const markup = renderToStaticMarkup(
      <TranscriptPanelContent
        entries={[]}
        hasTarget={false}
        socketError={null}
        status="disconnected"
      />
    );
    expect(markup).toContain('No attempt selected');
  });

  test('surfaces a socket error as visible state, never swallowed', () => {
    const markup = renderToStaticMarkup(
      <TranscriptPanelContent
        entries={[]}
        hasTarget={true}
        socketError={{ kind: 'frame-decode', message: 'stream fell behind', cause: null }}
        status="reconnecting"
      />
    );
    expect(markup).toContain('stream fell behind');
    expect(markup).toContain('Reconnecting');
  });

  test('renders the ordered transcript entries inside the bounded scroll region', () => {
    const entries: TranscriptEntry[] = [
      {
        type: 'event',
        key: 'seq:0',
        event: event({ kind: 'Message', role: { role: 'Assistant' }, text: 'hi' }, 0),
      },
      {
        type: 'event',
        key: 'seq:1',
        event: event({ kind: 'ToolCall', tool: 'read_file', call_id: 'c1', input: {} }, 1),
      },
    ];
    const markup = renderToStaticMarkup(
      <TranscriptPanelContent
        entries={entries}
        hasTarget={true}
        socketError={null}
        status="connected"
      />
    );
    expect(markup).toContain('hi');
    expect(markup).toContain('Tool call · read_file');
    expect(markup).toContain('Live');
    expect(markup).toContain('data-testid="transcript-scroll"');
  });
});

describe('TranscriptEntryRow', () => {
  test('a coalesced delta stream renders as ONE live streaming row with the accumulated text', () => {
    const entry: TranscriptEntry = {
      type: 'stream',
      key: 'stream:agent:msg_1',
      messageId: 'msg_1',
      text: ' the diagnostics crate',
      event: event({ kind: 'Delta', message_id: 'msg_1', text_fragment: ' crate' }, null),
    };
    const markup = renderToStaticMarkup(<TranscriptEntryRow entry={entry} />);
    expect(markup).toContain('Message · streaming');
    expect(markup).toContain(' the diagnostics crate');
    expect(markup).toContain('data-ephemeral="true"');
    expect(markup).toContain('live');
  });

  test('a note run renders as ONE counted progress row, not a row per chunk', () => {
    const noteEvent = event(
      { kind: 'Progress', detail: { detail: 'Note', text: 'tool_call_delta' } },
      5
    );
    const entry: TranscriptEntry = {
      type: 'notes',
      key: 'seq:1',
      text: 'tool_call_delta',
      count: 5,
      seqs: [1, 2, 3, 4, 5],
      event: noteEvent,
    };
    const markup = renderToStaticMarkup(<TranscriptEntryRow entry={entry} />);
    expect(markup).toContain('tool_call_delta ×5');
  });

  test('a reasoning_item raw renders its summary text, never the encrypted blob', () => {
    const markup = renderToStaticMarkup(
      <TranscriptEventRow
        event={event(
          {
            kind: 'Raw',
            source: 'event/item',
            value: {
              type: 'reasoning_item',
              item: {
                id: 'rs_1',
                encrypted_content: 'gAAAA-opaque-blob',
                content: [],
                summary: [{ type: 'summary_text', text: '**Planning for review**' }],
              },
            },
          },
          7
        )}
      />
    );
    expect(markup).toContain('Reasoning');
    expect(markup).toContain('**Planning for review**');
    expect(markup).not.toContain('gAAAA-opaque-blob');
    expect(markup).not.toContain('encrypted_content');
  });

  test('a genuinely unknown raw value keeps the generic JSON fallback', () => {
    const markup = renderToStaticMarkup(
      <TranscriptEventRow
        event={event({ kind: 'Raw', source: 'event/mystery', value: { widget: 7 } }, 8)}
      />
    );
    expect(markup).toContain('Raw · event/mystery');
    expect(markup).toContain('widget');
  });
});

describe('TranscriptEventRow', () => {
  test('flags an ephemeral delta distinctly from a persisted message', () => {
    const delta = renderToStaticMarkup(
      <TranscriptEventRow
        event={event({ kind: 'Delta', message_id: 'm', text_fragment: 'wor' }, null)}
      />
    );
    expect(delta).toContain('data-ephemeral="true"');
    expect(delta).toContain('wor');

    const message = renderToStaticMarkup(
      <TranscriptEventRow
        event={event({ kind: 'Message', role: { role: 'Assistant' }, text: 'done' }, 2)}
      />
    );
    expect(message).toContain('data-ephemeral="false"');
  });

  test('marks an errored tool result distinctly', () => {
    const markup = renderToStaticMarkup(
      <TranscriptEventRow
        event={event(
          { kind: 'ToolResult', call_id: 'c1', output: { message: 'boom' }, is_error: true },
          3
        )}
      />
    );
    expect(markup).toContain('Tool result');
    expect(markup).toContain('boom');
  });
});

describe('sortAttempts', () => {
  test('sorts ascending by activity then attempt', () => {
    const rows: AttemptCapabilities[] = [
      { activityId: 4, attempt: 2, capabilities: { supported: [] } },
      { activityId: 3, attempt: 2, capabilities: { supported: [] } },
      { activityId: 3, attempt: 1, capabilities: { supported: [] } },
    ];
    expect(sortAttempts(rows).map((row) => `${row.activityId}:${row.attempt}`)).toEqual([
      '3:1',
      '3:2',
      '4:2',
    ]);
  });
});

describe('TranscriptPanelContent retained backfill', () => {
  test('surfaces a retained-backfill failure as visible state', () => {
    const markup = renderToStaticMarkup(
      <TranscriptPanelContent
        backfillError={new Error('boom')}
        entries={[]}
        hasTarget={true}
        socketError={null}
        status="connected"
      />
    );

    expect(markup).toContain('data-testid="transcript-backfill-error"');
    expect(markup).toContain('boom');
    expect(markup).toContain('live-socket replay');
  });
});
