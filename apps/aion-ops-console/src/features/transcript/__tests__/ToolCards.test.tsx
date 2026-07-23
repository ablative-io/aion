import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { ActivityEvent } from '@/types';

import { toolArgumentFields, toolError } from '../components/ToolCard';
import { TranscriptEntryRow } from '../components/TranscriptEventRow';
import {
  foldTranscriptEvent,
  type TranscriptEntry,
  type TranscriptToolEntry,
} from '../lib/entries';

const AGENT_ID = '00000000-0000-0000-0000-0000000000aa';

function rawEvent(method: string, params: Record<string, unknown>, storeSeq: number | null) {
  return event(
    {
      kind: 'Raw',
      source: method,
      value: params,
    },
    storeSeq
  );
}

function event(kind: ActivityEvent['kind'], storeSeq: number | null): ActivityEvent {
  return {
    workflow_id: '00000000-0000-0000-0000-000000000001',
    activity_id: 3,
    attempt: 1,
    agent_id: AGENT_ID,
    agent_role: 'orchestrator',
    emitted_at: '2026-07-01T00:00:00Z',
    worker_seq: storeSeq ?? 99,
    store_seq: storeSeq,
    ephemeral: storeSeq === null,
    kind,
  };
}

function call(name: string, callId: string, args: unknown, storeSeq = 1): ActivityEvent {
  return rawEvent(
    'event/toolCall',
    {
      type: 'tool_call',
      call_id: callId,
      name,
      arguments: args,
      kind: 'function',
    },
    storeSeq
  );
}

function result(
  name: string,
  callId: string,
  output: unknown,
  storeSeq = 2,
  durationMs = 12
): ActivityEvent {
  return rawEvent(
    'event/toolResult',
    {
      type: 'tool_result',
      tool_call_id: callId,
      tool_name: name,
      output,
      duration_ms: durationMs,
    },
    storeSeq
  );
}

function fold(events: ActivityEvent[]): TranscriptEntry[] {
  return events.reduce<TranscriptEntry[]>(
    (entries, next, index) => foldTranscriptEvent(entries, next, index),
    []
  );
}

function toolEntry(events: ActivityEvent[]): TranscriptToolEntry {
  const entry = fold(events)[0];
  if (entry?.type !== 'tool') {
    throw new Error('expected paired tool entry');
  }
  return entry;
}

describe('tool event pairing', () => {
  test('pairs call and result by correlation id and removes the raw result row', () => {
    const entries = fold([
      call('read', 'call-1', { path: '/tmp/a' }),
      result('read', 'call-1', { kind: 'text', content: 'hello' }),
    ]);

    expect(entries).toHaveLength(1);
    expect(entries[0]?.type).toBe('tool');
    const paired = entries[0] as TranscriptToolEntry;
    expect(paired.call?.name).toBe('read');
    expect(paired.result?.durationMs).toBe(12);
  });

  test('pairs an out-of-order result when its call arrives later', () => {
    const paired = toolEntry([
      result('bash', 'call-2', { exit_code: 0 }, 2),
      call('bash', 'call-2', { command: 'pwd' }, 1),
    ]);

    expect(paired.call?.name).toBe('bash');
    expect(paired.result?.name).toBe('bash');
    expect(paired.key).toBe('tool:call-2');
  });

  test('coalesces streamed tool arguments into one pending row until the call finalizes', () => {
    const delta = (fragment: string) =>
      rawEvent(
        'event/progress',
        {
          type: 'tool_call_delta',
          item_id: 'item-1',
          call_id: 'stream-1',
          name: 'search',
          arguments_delta: fragment,
          kind: 'function',
        },
        null
      );
    const streaming = fold([delta('{"pattern"'), delta(':"needle"}')]);

    expect(streaming).toHaveLength(1);
    expect(streaming[0]?.type).toBe('tool-stream');
    expect(streaming[0]?.type === 'tool-stream' ? streaming[0].argumentsText : '').toBe(
      '{"pattern":"needle"}'
    );

    const finalized = foldTranscriptEvent(
      streaming,
      call('search', 'stream-1', { pattern: 'needle' }),
      2
    );
    expect(finalized).toHaveLength(1);
    expect(finalized[0]?.type).toBe('tool');
  });

  test('leaves an unpaired call pending', () => {
    const entry = toolEntry([call('write', 'call-3', { path: '/tmp/a' })]);
    const markup = renderToStaticMarkup(<TranscriptEntryRow entry={entry} />);

    expect(entry.result).toBeNull();
    expect(markup).toContain('pending');
    expect(markup).toContain('Waiting for result');
  });
});

describe('tool cards', () => {
  test('detects the cross-tool error envelope before read dispatch', () => {
    const output = {
      kind: 'text',
      content: 'must not be presented as success',
      error: {
        kind: 'permission_denied',
        message: 'read refused',
        detail: { path: '/secret' },
        guidance: 'Choose a workspace path.',
      },
    };
    expect(toolError(output)).toEqual(output.error);

    const markup = renderToStaticMarkup(
      <TranscriptEntryRow
        entry={toolEntry([call('read', 'err-1', {}), result('read', 'err-1', output)])}
      />
    );
    expect(markup).toContain('permission_denied');
    expect(markup).toContain('read refused');
    expect(markup).toContain('Details and guidance');
  });

  test('renders representative read text payload fields', () => {
    const entry = toolEntry([
      call('read', 'read-1', { path: '/repo/a.ts' }),
      result('read', 'read-1', {
        kind: 'text',
        path: '/repo/a.ts',
        content: 'const answer = 42;',
        returned_lines: 1,
        total_lines: 90,
        truncated: true,
        warnings: [{ message: 'line was long' }],
        spool_ref: 'spool/read-1',
      }),
    ]);
    const markup = renderToStaticMarkup(<TranscriptEntryRow entry={entry} />);

    expect(markup).toContain('const answer = 42;');
    expect(markup).toContain('90 total lines');
    expect(markup).toContain('Read window truncated after 1 lines');
    expect(markup).toContain('line was long');
    expect(markup).toContain('Output truncated');
    expect(markup).toContain('12 ms');
  });

  test('renders bash command, stdout, stderr, and exit code', () => {
    const entry = toolEntry([
      call('bash', 'bash-1', { command: 'bun test' }),
      result('bash', 'bash-1', {
        exit_code: 1,
        stdout: '1 pass',
        stderr: '1 fail',
        timed_out: false,
      }),
    ]);
    const markup = renderToStaticMarkup(<TranscriptEntryRow entry={entry} />);

    expect(markup).toContain('$ bun test');
    expect(markup).toContain('1 pass');
    expect(markup).toContain('1 fail');
    expect(markup).toContain('exit 1');
  });

  test('unknown tools use the indent-2 generic JSON fallback', () => {
    const entry = toolEntry([
      call('future_tool', 'future-1', { mode: 'new' }),
      result('future_tool', 'future-1', { future_field: { enabled: true } }),
    ]);
    const markup = renderToStaticMarkup(<TranscriptEntryRow entry={entry} />);

    expect(markup).toContain('&quot;future_field&quot;: {');
    expect(markup).toContain('  &quot;enabled&quot;: true');
  });

  test('uses description as subtitle and excludes both envelope keys from the parameter table', () => {
    const args = {
      path: '/repo/a.ts',
      tool_use_description: 'Read the source before editing.',
      tool_use_metadata: { trace: 'hidden-table-value' },
    };
    expect(toolArgumentFields(args)).toEqual([['path', '/repo/a.ts']]);

    const markup = renderToStaticMarkup(
      <TranscriptEntryRow entry={toolEntry([call('read', 'env-1', args)])} />
    );
    expect(markup).toContain('Read the source before editing.');
    expect(markup).toContain('View raw events');
  });
});
