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
  'workflow-failure': { rail: 'border-l-red-400/70 text-red-300', glyph: '✕' },
  'workflow-stuck': { rail: 'border-l-amber-400/70 text-amber-300', glyph: '◷' },
  'dead-worker': { rail: 'border-l-red-400/70 text-red-300', glyph: '☠' },
  'outbox-failed': { rail: 'border-l-amber-400/70 text-amber-300', glyph: '⇄' },
  'fenced-rejection': { rail: 'border-l-violet-400/70 text-violet-300', glyph: '⊘' },
  'shard-adoption': { rail: 'border-l-sky-400/70 text-sky-300', glyph: '★' },
};

export function IncidentCard({ incident }: IncidentCardProps) {
  const tone = KIND_TONE[incident.kind];

  return (
    <article
      className={`flex items-center gap-4 border border-[var(--border-default)] border-l-4 ${tone.rail} bg-[var(--surface-default)] px-4 py-3`}
      data-incident-kind={incident.kind}
    >
      <span aria-hidden className={`font-mono text-lg ${tone.rail}`}>
        {tone.glyph}
      </span>

      <div className="min-w-0 flex-1">
        <p className="truncate font-medium text-[var(--text-primary)] text-sm">{incident.title}</p>
        <p className="truncate text-[var(--text-muted)] text-xs">{incident.detail}</p>
        {incident.at === null ? null : (
          <p className="text-[var(--text-muted)] text-xs">{incident.at}</p>
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
    <Link
      className="shrink-0 text-[var(--accent-cyan)] text-sm underline-offset-4 hover:underline"
      to={href}
    >
      {incident.actionLabel} →
    </Link>
  );
}
