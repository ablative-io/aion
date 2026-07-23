import { memo, useState } from 'react';

import { Badge, Button } from '@/components/ui';
import { cn } from '@/lib/utils';
import type { ActivityEvent, ActivityEventKind, ProgressDetail, StopKind } from '@/types';

import type { TranscriptEntry } from '../hooks/useTranscript';
import { parseReasoningItem } from '../lib/reasoning';
import { prettyJson } from '../lib/toolEvents';
import { StreamingToolCard, ToolCard } from './ToolCard';

/**
 * Presentational transcript rows (NOI-7). {@link TranscriptEntryRow} renders one
 * folded {@link TranscriptEntry} — a classified event (Message / ToolCall /
 * ToolResult / Progress / Stop / Reasoning / Raw), a live coalesced token stream,
 * or a counted progress-note run — with the agent role, a kind label, and the
 * payload. REAL DATA ONLY: every field comes straight from the streamed events.
 *
 * The row is memoized on its (immutable) `entry` reference so a streamed token only
 * re-runs the presentation + `JSON.stringify` for the ONE row whose text grew, not
 * every row on the list. An oversized body ({@link BODY_CLAMP_THRESHOLD_CHARS}) is
 * clamped to a fixed height with a "Show all" toggle — nothing is ever dropped, the
 * full payload is always in the DOM and expands in place.
 */

/** Bodies longer than this render collapsed behind a "Show all" toggle. */
const BODY_CLAMP_THRESHOLD_CHARS = 4000;

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

const MUTED_TONE = 'text-muted-foreground';
const PRIMARY_TONE = 'text-foreground';

/** Presentation for a classified single event. */
function eventPresentation(kind: ActivityEventKind): RowPresentation {
  switch (kind.kind) {
    case 'Message':
      return { label: `Message · ${kind.role.role}`, tone: PRIMARY_TONE, body: kind.text };
    case 'ToolCall':
      return {
        label: `Tool call · ${kind.tool}`,
        tone: 'text-live',
        body: `${kind.call_id}\n${stringify(kind.input)}`,
      };
    case 'ToolResult':
      return {
        label: 'Tool result',
        tone: kind.is_error ? 'text-danger' : 'text-success',
        body: `${kind.call_id}\n${stringify(kind.output)}`,
      };
    case 'Progress':
      return { label: 'Progress', tone: MUTED_TONE, body: progressBody(kind.detail) };
    case 'Stop':
      return {
        label: `Stop · ${kind.reason.stop}`,
        tone: 'text-warning',
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
      tone: 'text-special',
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
      return {
        label: entry.streamKind === 'thinking' ? 'Thinking · streaming' : 'Message · streaming',
        tone: entry.streamKind === 'thinking' ? 'text-special' : PRIMARY_TONE,
        body: entry.text,
      };
    case 'tool':
      return { label: 'Tool', tone: 'text-live', body: '' };
    case 'tool-stream':
      return { label: 'Tool · streaming', tone: 'text-live', body: entry.argumentsText };
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

/** Whether a body is large enough to render collapsed behind a "Show all" toggle. */
export function isBodyClamped(body: string): boolean {
  return body.length > BODY_CLAMP_THRESHOLD_CHARS;
}

/** Human-readable size of a clamped body for the toggle label, e.g. `"812 lines · 47 KB"`. */
export function bodySizeLabel(body: string): string {
  const lines = body.split('\n').length;
  const kb = new TextEncoder().encode(body).length / 1024;
  const kbLabel = kb >= 10 ? String(Math.round(kb)) : kb.toFixed(1);
  const linesLabel = lines === 1 ? '1 line' : `${lines} lines`;
  return `${linesLabel} · ${kbLabel} KB`;
}

const PRE_CLASSES = 'whitespace-pre-wrap break-words font-mono text-foreground text-xs';

export type TranscriptBodyProps = {
  body: string;
  /** Only meaningful when the body is clamped; ignored for short bodies. */
  expanded: boolean;
  onToggle: () => void;
};

/**
 * The payload body. A short body renders as a plain `<pre>`. A large body renders
 * clamped to a fixed height with a bottom fade and a toggle; the FULL text is always
 * present in the DOM (the clamp is CSS `overflow-hidden`), so expanding never fetches
 * or reveals anything that was dropped — it just lifts the height cap.
 */
export function TranscriptBody({ body, expanded, onToggle }: TranscriptBodyProps) {
  if (!isBodyClamped(body)) {
    return <pre className={PRE_CLASSES}>{body}</pre>;
  }

  return (
    <div>
      {expanded ? (
        <pre className={PRE_CLASSES}>{body}</pre>
      ) : (
        <div className="relative max-h-64 overflow-hidden">
          <pre className={PRE_CLASSES}>{body}</pre>
          <div
            aria-hidden
            className="pointer-events-none absolute inset-x-0 bottom-0 h-12 bg-gradient-to-t from-surface-default to-transparent"
          />
        </div>
      )}
      <Button
        className="mt-2 h-7 px-2 text-xs"
        onClick={onToggle}
        size="sm"
        type="button"
        variant="ghost"
      >
        {expanded ? 'Collapse' : `Show all (${bodySizeLabel(body)})`}
      </Button>
    </div>
  );
}

/** One folded transcript entry as a list row. */
function TranscriptEntryRowImpl({ entry, isNew = false }: TranscriptEntryRowProps) {
  const { event } = entry;
  const [expanded, setExpanded] = useState(false);
  const animation = isNew && 'fade-in-0 slide-in-from-bottom-2 animate-in duration-200';

  if (entry.type === 'tool') {
    return (
      <li
        className={cn('pb-2', animation)}
        data-entry-key={entry.key}
        data-entry-type={entry.type}
        data-event-kind={event.kind.kind}
        data-ephemeral={event.ephemeral}
      >
        <ToolCard entry={entry} />
      </li>
    );
  }

  if (entry.type === 'tool-stream') {
    return (
      <li
        className={cn('pb-2', animation)}
        data-entry-key={entry.key}
        data-entry-type={entry.type}
        data-event-kind={event.kind.kind}
        data-ephemeral={event.ephemeral}
      >
        <StreamingToolCard entry={entry} />
      </li>
    );
  }

  const { label, tone, body } = entryPresentation(entry);
  const contentBlock = textBlockKind(entry);

  return (
    <li
      className={cn('pb-2', animation)}
      data-entry-key={entry.key}
      data-entry-type={entry.type}
      data-event-kind={event.kind.kind}
      data-ephemeral={event.ephemeral}
    >
      <div className="rounded-lg border border-border bg-surface-default p-3">
        <div className="mb-1 flex items-center justify-between gap-2">
          <span className={`font-medium text-xs ${tone}`}>{label}</span>
          <span className="flex items-center gap-2 text-[10px] text-muted-foreground">
            {event.agent_role}
            {event.ephemeral ? (
              <Badge className="h-4 px-1 text-[9px]" variant="outline">
                live
              </Badge>
            ) : null}
          </span>
        </div>
        {contentBlock === null ? (
          <TranscriptBody body={body} expanded={expanded} onToggle={() => setExpanded((v) => !v)} />
        ) : (
          <FlowingTextBlock
            expanded={expanded}
            event={event}
            muted={contentBlock === 'thinking'}
            onToggle={() => setExpanded((value) => !value)}
            text={body}
          />
        )}
      </div>
    </li>
  );
}

function textBlockKind(entry: TranscriptEntry): 'text' | 'thinking' | null {
  if (entry.type === 'stream') {
    return entry.streamKind ?? 'text';
  }
  if (entry.type !== 'event') {
    return null;
  }
  if (entry.event.kind.kind === 'Message') {
    return 'text';
  }
  if (entry.event.kind.kind === 'Raw' && parseReasoningItem(entry.event.kind.value) !== null) {
    return 'thinking';
  }
  return null;
}

/** Human prose stays prose: no JSON row and no monospace payload treatment. */
function FlowingTextBlock({
  event,
  expanded,
  muted,
  onToggle,
  text,
}: {
  event: ActivityEvent;
  expanded: boolean;
  muted: boolean;
  onToggle: () => void;
  text: string;
}) {
  return (
    <div className={cn('space-y-2', muted && 'rounded-md bg-surface-subtle p-2')}>
      <FlowingTextBody expanded={expanded} muted={muted} onToggle={onToggle} text={text} />
      <details className="text-xs">
        <summary className="cursor-pointer text-muted-foreground">View raw event</summary>
        <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md bg-surface-subtle p-2 font-mono text-xs not-italic">
          {prettyJson(rawEventForDisplay(event))}
        </pre>
      </details>
    </div>
  );
}

function FlowingTextBody({
  expanded,
  muted,
  onToggle,
  text,
}: {
  expanded: boolean;
  muted: boolean;
  onToggle: () => void;
  text: string;
}) {
  const textClass = cn(
    'whitespace-pre-wrap break-words text-sm leading-relaxed',
    muted ? 'text-muted-foreground italic' : 'text-foreground'
  );
  if (!isBodyClamped(text)) {
    return <div className={textClass}>{text}</div>;
  }
  return (
    <div>
      <div className={cn('relative', !expanded && 'max-h-64 overflow-hidden')}>
        <div className={textClass}>{text}</div>
        {expanded ? null : (
          <div
            aria-hidden
            className="pointer-events-none absolute inset-x-0 bottom-0 h-12 bg-gradient-to-t from-surface-default to-transparent"
          />
        )}
      </div>
      <Button
        className="mt-2 h-7 px-2 text-xs"
        onClick={onToggle}
        size="sm"
        type="button"
        variant="ghost"
      >
        {expanded ? 'Collapse' : `Show all (${bodySizeLabel(text)})`}
      </Button>
    </div>
  );
}

/** Preserve the established rule that provider-encrypted reasoning never enters the DOM. */
function rawEventForDisplay(event: ActivityEvent): ActivityEvent {
  if (event.kind.kind !== 'Raw' || parseReasoningItem(event.kind.value) === null) {
    return event;
  }
  return {
    ...event,
    kind: {
      kind: 'Raw',
      source: event.kind.source,
      value: '[provider reasoning payload omitted; summary rendered above]',
    },
  };
}

/**
 * Memoized on the (immutable) `entry` reference. The fold hands each row a frozen
 * entry object, so a streamed token produces a NEW reference only for the one row
 * that grew; every other row keeps its reference and is skipped. `isNew` is
 * deliberately excluded from the comparator: it drives a mount-only CSS entrance
 * animation (`animate-in`) that has already played by the time the flag flips to
 * `false`, so re-rendering the row just to observe that flip would be wasted work.
 */
export const TranscriptEntryRow = memo(
  TranscriptEntryRowImpl,
  (prev, next) => prev.entry === next.entry
);

/** A single classified event as a row (the common `event` entry). */
export function TranscriptEventRow({ event }: TranscriptEventRowProps) {
  const key = typeof event.store_seq === 'number' ? `seq:${event.store_seq}` : 'live';
  return <TranscriptEntryRow entry={{ type: 'event', key, event }} />;
}
