import type { TimelineEntry as TimelineEntryModel } from '../types';
import { PayloadView } from './PayloadView';

type DetailPanelBodyProps = {
  entry: TimelineEntryModel;
};

/**
 * The decoded inspector CONTENT for a selected timeline entry: the event envelope
 * (type / seq / recorded_at / workflow_id) plus a best-effort payload view with a
 * raw-bytes fallback (invariant 1). Extracted from the old slide-out aside so the
 * bottom-docked {@link DetailSheet} can reuse the exact same rendering — the
 * identity/close chrome lives on the sheet's entity-pill header, not here.
 */
function DetailPanelBody({ entry }: DetailPanelBodyProps) {
  const lastEvent = entry.events.at(-1) ?? entry.events[0];
  const eventType = lastEvent?.type ?? entry.kind;

  return (
    <div className="flex flex-col gap-4">
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
        <h3 className="mb-2 font-medium text-secondary-foreground text-xs uppercase tracking-wide">
          Payload
        </h3>
        <PayloadView label="Decoded payload" payload={entry.payload ?? lastEvent?.data ?? null} />
      </div>
    </div>
  );
}

function Field({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-wrap items-baseline justify-between gap-2">
      <dt className="text-muted-foreground text-xs uppercase tracking-wide">{label}</dt>
      <dd className="min-w-0 break-words text-right font-mono text-secondary-foreground text-xs">
        {value}
      </dd>
    </div>
  );
}

export { DetailPanelBody };
