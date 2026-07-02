import { Badge } from '@/components/ui';
import { cn } from '@/lib/utils';
import type { ActivityEvent, ActivityEventKind, ProgressDetail, StopKind } from '@/types';

import type { TranscriptEntry } from '../hooks/useTranscript';
import { parseReasoningItem } from '../lib/reasoning';

/**
 * Presentational transcript rows (NOI-7). {@link TranscriptEntryRow} renders one
 * folded {@link TranscriptEntry} — a classified event (Message / ToolCall /
 * ToolResult / Progress / Stop / Reasoning / Raw), a live coalesced token stream,
 * or a counted progress-note run — with the agent role, a kind label, and the
 * payload. REAL DATA ONLY: every field comes straight from the streamed events.
 */

export type TranscriptEntryRowProps = {
  entry: TranscriptEntry;
  /** Apply the subtle entrance animation (a newly-arrived live entry). */
  isNew?: boolean;
};

export type TranscriptEventRowProps = {
  event: ActivityEvent;
};

/** How one entry renders: a short human label, its accent, and the body text. */
type RowPresentation = { label: string; tone: string; body: string };

const MUTED_TONE = 'text-[var(--text-muted)]';
const PRIMARY_TONE = 'text-[var(--text-primary)]';

/** Presentation for a classified single event. */
function eventPresentation(kind: ActivityEventKind): RowPresentation {
  switch (kind.kind) {
    case 'Message':
      return { label: `Message · ${kind.role.role}`, tone: PRIMARY_TONE, body: kind.text };
    case 'ToolCall':
      return {
        label: `Tool call · ${kind.tool}`,
        tone: 'text-sky-500',
        body: `${kind.call_id}\n${stringify(kind.input)}`,
      };
    case 'ToolResult':
      return {
        label: 'Tool result',
        tone: kind.is_error ? 'text-destructive' : 'text-emerald-500',
        body: `${kind.call_id}\n${stringify(kind.output)}`,
      };
    case 'Progress':
      return { label: 'Progress', tone: MUTED_TONE, body: progressBody(kind.detail) };
    case 'Stop':
      return {
        label: `Stop · ${kind.reason.stop}`,
        tone: 'text-amber-500',
        body: stopBody(kind.reason),
      };
    case 'Delta':
      return { label: 'Delta', tone: MUTED_TONE, body: kind.text_fragment };
    case 'Raw':
      return rawPresentation(kind);
  }
}

/**
 * Presentation for a raw passthrough. A `reasoning_item` payload renders as a
 * proper Reasoning entry showing only its human-readable summary text(s) — never
 * the raw JSON with the provider's encrypted blob. Genuinely unknown raw values
 * keep the generic JSON fallback so nothing is silently dropped.
 */
function rawPresentation(kind: Extract<ActivityEventKind, { kind: 'Raw' }>): RowPresentation {
  const reasoning = parseReasoningItem(kind.value);
  if (reasoning !== null) {
    return {
      label: 'Reasoning',
      tone: 'text-violet-500',
      body:
        reasoning.summaries.length > 0
          ? reasoning.summaries.join('\n\n')
          : '(no reasoning summary provided)',
    };
  }
  return { label: `Raw · ${kind.source}`, tone: MUTED_TONE, body: stringify(kind.value) };
}

/** Presentation for one folded entry. */
function entryPresentation(entry: TranscriptEntry): RowPresentation {
  switch (entry.type) {
    case 'event':
      return eventPresentation(entry.event.kind);
    case 'stream':
      return { label: 'Message · streaming', tone: PRIMARY_TONE, body: entry.text };
    case 'notes':
      return {
        label: 'Progress',
        tone: MUTED_TONE,
        body: entry.count > 1 ? `${entry.text} ×${entry.count}` : entry.text,
      };
  }
}

function progressBody(detail: ProgressDetail): string {
  if (detail.detail === 'Note') {
    return detail.text;
  }
  const input = detail.input_tokens ?? '—';
  const output = detail.output_tokens ?? '—';
  return `input ${String(input)} · output ${String(output)} tokens`;
}

function stopBody(reason: StopKind): string {
  if (reason.stop === 'Error') {
    return reason.message;
  }
  if (reason.stop === 'Other') {
    return reason.reason;
  }
  return reason.stop;
}

/** Serialize a JSON value compactly for display, never throwing on cyclic input. */
function stringify(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2) ?? String(value);
  } catch {
    return String(value);
  }
}

/** One folded transcript entry as a list row. */
export function TranscriptEntryRow({ entry, isNew = false }: TranscriptEntryRowProps) {
  const { label, tone, body } = entryPresentation(entry);
  const { event } = entry;

  return (
    <li
      className={cn('pb-2', isNew && 'fade-in-0 slide-in-from-bottom-2 animate-in duration-200')}
      data-entry-key={entry.key}
      data-entry-type={entry.type}
      data-event-kind={event.kind.kind}
      data-ephemeral={event.ephemeral}
    >
      <div className="rounded-lg border border-[var(--border-default)] bg-[var(--surface-default)] p-3">
        <div className="mb-1 flex items-center justify-between gap-2">
          <span className={`font-medium text-xs ${tone}`}>{label}</span>
          <span className="flex items-center gap-2 text-[10px] text-[var(--text-muted)]">
            {event.agent_role}
            {event.ephemeral ? (
              <Badge className="h-4 px-1 text-[9px]" variant="outline">
                live
              </Badge>
            ) : null}
          </span>
        </div>
        <pre className="whitespace-pre-wrap break-words font-mono text-[var(--text-primary)] text-xs">
          {body}
        </pre>
      </div>
    </li>
  );
}

/** A single classified event as a row (the common `event` entry). */
export function TranscriptEventRow({ event }: TranscriptEventRowProps) {
  const key = typeof event.store_seq === 'number' ? `seq:${event.store_seq}` : 'live';
  return <TranscriptEntryRow entry={{ type: 'event', key, event }} />;
}
