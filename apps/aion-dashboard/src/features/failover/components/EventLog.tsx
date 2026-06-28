import { useMemo } from 'react';

import { eventRecordedAt, eventSequence } from '@/features/workflow-detail/lib/timeline';
import type { Event } from '@/types';

/**
 * Flat, seq-ordered, bounded event log (DEMO-VIEW-PLAN §1). Deliberately does NOT
 * use `projectTimeline` — that engine throws `assertNever` on unhandled variants,
 * which would white-screen the demo on an unexpected event. Here every wire event
 * renders as a generic typed row, and synthesized health/adoption rows are merged
 * in by sequence so the kill + adoption + resumed-completion story reads top to
 * bottom in one column. Newest at top, bounded ring (no unbounded growth on stage).
 */

const MAX_ROWS = 60;

export type SyntheticRow = {
  /** Stable id for React keys. */
  id: string;
  /** Sequence used to interleave with wire events; synthetic rows pin a seq. */
  sequence: number;
  /** Epoch ms when the synthetic event was observed. */
  observedAt: number;
  kind: 'health' | 'adoption';
  /** Glyph rendered at the row head (⚠ for health, ★ for adoption). */
  glyph: string;
  label: string;
  detail: string;
};

type LogRow = {
  id: string;
  sequence: number;
  glyph: string;
  label: string;
  detail: string;
  tone: 'event' | 'health' | 'adoption';
};

export type EventLogProps = {
  /** Seq-ordered, deduped wire events (history + live merged). */
  events: readonly Event[];
  /** Synthesized health/adoption rows to interleave by sequence. */
  synthetic: readonly SyntheticRow[];
  /** Epoch ms of the first event, to render relative `+N.Ns` offsets. */
  startedAt: number | null;
};

function wireRow(event: Event): LogRow {
  const sequence = eventSequence(event);

  return {
    id: `wire:${sequence}:${event.type}`,
    sequence,
    glyph: '·',
    label: event.type,
    detail: detailForEvent(event),
    tone: 'event',
  };
}

function detailForEvent(event: Event): string {
  switch (event.type) {
    case 'WorkflowStarted':
      return event.data.workflow_type;
    case 'ActivityScheduled':
    case 'ActivityStarted':
    case 'ActivityCompleted':
    case 'ActivityFailed':
    case 'ActivityCancelled':
      return `ordinal ${event.data.activity_id}`;
    default:
      // Unknown / unhandled variant: render generically, never crash.
      return '';
  }
}

function syntheticRow(row: SyntheticRow): LogRow {
  return {
    id: row.id,
    sequence: row.sequence,
    glyph: row.glyph,
    label: row.label,
    detail: row.detail,
    tone: row.kind,
  };
}

function relativeLabel(observedAtSeq: number, startedAt: number | null): string {
  // Wire events carry a real timestamp; we key the column on recorded_at below.
  return startedAt === null ? `#${observedAtSeq}` : `#${observedAtSeq}`;
}

const TONE_COLOR: Record<LogRow['tone'], string> = {
  event: 'var(--text-secondary)',
  health: 'var(--destructive)',
  adoption: 'var(--accent-cyan)',
};

export function EventLog({ events, synthetic, startedAt }: EventLogProps) {
  const rows = useMemo<LogRow[]>(() => {
    const merged: LogRow[] = [...events.map(wireRow), ...synthetic.map(syntheticRow)];
    merged.sort((left, right) => left.sequence - right.sequence);
    // Newest at top, bounded ring.
    return merged.slice(-MAX_ROWS).reverse();
  }, [events, synthetic]);

  const firstTimestamp = useMemo(() => {
    const first = events.at(0);
    return first ? Date.parse(eventRecordedAt(first)) : null;
  }, [events]);

  function offsetFor(row: LogRow): string {
    const wire = events.find((event) => eventSequence(event) === row.sequence);

    if (wire === undefined || firstTimestamp === null) {
      return relativeLabel(row.sequence, startedAt);
    }

    const delta = Date.parse(eventRecordedAt(wire)) - firstTimestamp;
    return `+${(delta / 1000).toFixed(1)}s`;
  }

  return (
    <div className="flex flex-col gap-2 rounded-md border border-[var(--border-default)] bg-[var(--card)] p-4">
      <span className="font-medium text-[0.7rem] text-[var(--text-muted)] uppercase tracking-[0.2em]">
        event log
      </span>

      {rows.length === 0 ? (
        <p className="text-[var(--text-muted)] text-sm">no events yet</p>
      ) : (
        <ol className="flex flex-col gap-1 font-mono text-xs">
          {rows.map((row) => (
            <li className="flex items-baseline gap-3" key={row.id}>
              <span className="w-14 shrink-0 text-right text-[var(--text-muted)]">
                {offsetFor(row)}
              </span>
              <span aria-hidden="true" style={{ color: TONE_COLOR[row.tone] }}>
                {row.glyph}
              </span>
              <span className="w-44 shrink-0" style={{ color: TONE_COLOR[row.tone] }}>
                {row.label}
              </span>
              <span className="truncate text-[var(--text-muted)]">{row.detail}</span>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}
