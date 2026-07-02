import { Link } from 'react-router';

import { failoverHref, workflowDetailHref } from '@/app/routePaths';

import type { Incident, IncidentKind } from '../lib/incidents';

export type IncidentCardProps = {
  incident: Incident;
};

type KindTone = {
  /** Left rail + glyph color: state via color, per hand-plane. */
  rail: string;
  glyph: string;
};

// Tone map keyed on kind. Failures are red (act now), stuck is amber (watch),
// gated classes never reach a card (the view renders them as awaiting-support
// rows), but the map is total so a future promoted class is forced to pick a tone.
const KIND_TONE: Record<IncidentKind, KindTone> = {
  'workflow-failure': { rail: 'border-l-danger/70 text-danger', glyph: '✕' },
  'workflow-stuck': { rail: 'border-l-warning/70 text-warning', glyph: '◷' },
  'dead-worker': { rail: 'border-l-danger/70 text-danger', glyph: '☠' },
  'outbox-failed': { rail: 'border-l-warning/70 text-warning', glyph: '⇄' },
  'fenced-rejection': { rail: 'border-l-special/70 text-special', glyph: '⊘' },
  'shard-adoption': { rail: 'border-l-live/70 text-live', glyph: '★' },
};

export function IncidentCard({ incident }: IncidentCardProps) {
  const tone = KIND_TONE[incident.kind];

  return (
    <article
      className={`flex items-center gap-4 border border-border border-l-4 ${tone.rail} bg-surface-default px-4 py-3`}
      data-incident-kind={incident.kind}
    >
      <span aria-hidden className={`font-mono text-lg ${tone.rail}`}>
        {tone.glyph}
      </span>

      <div className="min-w-0 flex-1">
        <p className="truncate font-medium text-foreground text-sm">{incident.title}</p>
        <p className="truncate text-muted-foreground text-xs">{incident.detail}</p>
        {incident.at === null ? null : (
          <p className="text-muted-foreground text-xs">{incident.at}</p>
        )}
      </div>

      <IncidentAction incident={incident} />
    </article>
  );
}

function IncidentAction({ incident }: { incident: Incident }) {
  const href =
    incident.target.kind === 'failover'
      ? failoverHref()
      : workflowDetailHref(incident.target.workflowId, incident.target.seq);

  return (
    <Link className="shrink-0 text-primary text-sm underline-offset-4 hover:underline" to={href}>
      {incident.actionLabel} →
    </Link>
  );
}
