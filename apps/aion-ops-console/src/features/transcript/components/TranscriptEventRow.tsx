import { Badge } from '@/components/ui';
import type { ActivityEvent, ActivityEventKind, ProgressDetail } from '@/types';

/**
 * Presentational transcript event row (NOI-7). Renders one {@link ActivityEvent}
 * classified by its kind — Message / ToolCall / ToolResult / Progress / Stop /
 * Delta / Raw — with the agent role, a kind label, and the payload. REAL DATA
 * ONLY: every field comes straight from the streamed event.
 */

export type TranscriptEventRowProps = {
  event: ActivityEvent;
};

/** A short, human label + accent for each event kind. */
function kindLabel(kind: ActivityEventKind): { label: string; tone: string } {
  switch (kind.kind) {
    case 'Message':
      return { label: `Message · ${kind.role.role}`, tone: 'text-[var(--text-primary)]' };
    case 'ToolCall':
      return { label: `Tool call · ${kind.tool}`, tone: 'text-sky-500' };
    case 'ToolResult':
      return {
        label: 'Tool result',
        tone: kind.is_error ? 'text-destructive' : 'text-emerald-500',
      };
    case 'Progress':
      return { label: 'Progress', tone: 'text-[var(--text-muted)]' };
    case 'Stop':
      return { label: `Stop · ${kind.reason.stop}`, tone: 'text-amber-500' };
    case 'Delta':
      return { label: 'Delta', tone: 'text-[var(--text-muted)]' };
    case 'Raw':
      return { label: `Raw · ${kind.source}`, tone: 'text-[var(--text-muted)]' };
  }
}

/** Render the payload body for one event kind as readable text. */
function kindBody(kind: ActivityEventKind): string {
  switch (kind.kind) {
    case 'Message':
      return kind.text;
    case 'ToolCall':
      return `${kind.call_id}\n${stringify(kind.input)}`;
    case 'ToolResult':
      return `${kind.call_id}\n${stringify(kind.output)}`;
    case 'Progress':
      return progressBody(kind.detail);
    case 'Stop':
      return kind.reason.stop === 'Error'
        ? kind.reason.message
        : kind.reason.stop === 'Other'
          ? kind.reason.reason
          : kind.reason.stop;
    case 'Delta':
      return kind.text_fragment;
    case 'Raw':
      return stringify(kind.value);
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

/** Serialize a JSON value compactly for display, never throwing on cyclic input. */
function stringify(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2) ?? String(value);
  } catch {
    return String(value);
  }
}

export function TranscriptEventRow({ event }: TranscriptEventRowProps) {
  const { label, tone } = kindLabel(event.kind);
  const body = kindBody(event.kind);

  return (
    <li
      className="rounded-lg border border-[var(--border-default)] bg-[var(--surface-default)] p-3"
      data-event-kind={event.kind.kind}
      data-ephemeral={event.ephemeral}
    >
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
    </li>
  );
}
