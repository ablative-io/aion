import { X } from 'lucide-react';

import { Badge, Button } from '@/components/ui';

import type { TimelineEntry as TimelineEntryModel } from '../types';
import { PayloadView } from './PayloadView';

type DetailPanelProps = {
  entry: TimelineEntryModel | null;
  onClose: () => void;
};

/**
 * Slide-out inspector for a selected timeline entry. Shows the decoded event
 * envelope (seq / recorded_at / workflow_id / type) plus a best-effort payload
 * view with a raw-bytes fallback (invariant 1). Renders nothing when no entry is
 * selected so the panel does not occupy layout space.
 */
function DetailPanel({ entry, onClose }: DetailPanelProps) {
  if (entry === null) {
    return null;
  }

  const lastEvent = entry.events.at(-1) ?? entry.events[0];
  const eventType = lastEvent?.type ?? entry.kind;

  return (
    <aside
      aria-label={`Event detail for seq ${entry.sequence}`}
      className="flex w-full max-w-md shrink-0 flex-col gap-4 rounded-xl border border-[var(--border-default)] bg-[var(--surface-elevated)] p-4"
    >
      <header className="flex items-start justify-between gap-3">
        <div className="min-w-0 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <Badge variant="outline">seq {entry.envelope.seq}</Badge>
            <Badge variant="secondary">{entry.kind}</Badge>
          </div>
          <h2 className="break-words font-medium text-[var(--text-primary)] text-sm">
            {entry.summary}
          </h2>
        </div>
        <Button
          aria-label="Close detail panel"
          className="size-8 shrink-0 p-0"
          onClick={onClose}
          type="button"
          variant="ghost"
        >
          <X aria-hidden="true" className="size-4" />
        </Button>
      </header>

      <dl className="space-y-2 text-sm">
        <Field label="Event type" value={eventType} />
        <Field label="Sequence" value={String(entry.envelope.seq)} />
        <Field label="Recorded at" value={entry.envelope.recorded_at} />
        <Field label="Workflow" value={entry.envelope.workflow_id} />
        {entry.events.length > 1 ? (
          <Field label="Correlated events" value={`${entry.events.length} in this lane`} />
        ) : null}
      </dl>

      <div>
        <h3 className="mb-2 font-medium text-[var(--text-secondary)] text-xs uppercase tracking-wide">
          Payload
        </h3>
        <PayloadView label="Decoded payload" payload={entry.payload ?? lastEvent?.data ?? null} />
      </div>
    </aside>
  );
}

function Field({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-wrap items-baseline justify-between gap-2">
      <dt className="text-[var(--text-muted)] text-xs uppercase tracking-wide">{label}</dt>
      <dd className="min-w-0 break-words text-right font-mono text-[var(--text-secondary)] text-xs">
        {value}
      </dd>
    </div>
  );
}

export { DetailPanel };
