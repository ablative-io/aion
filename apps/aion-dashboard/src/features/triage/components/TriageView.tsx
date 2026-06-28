import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import type { ApiClient } from '@/lib/api';
import type { Namespace } from '@/types';

import { useIncidents } from '../hooks/useIncidents';
import type { GatedFeed } from '../lib/gatedFeeds';
import { IncidentCard } from './IncidentCard';

export type TriageViewProps = {
  namespace: Namespace | null;
  /** Override for tests; defaults to the hook's own default ApiClient. */
  client?: ApiClient;
};

/**
 * Three-AM triage view (plan §4.4 / slice S7). A ranked list of incident cards
 * an on-call engineer reads under pressure: failed/timed-out workflows and
 * stuck-running ones, each with a single deep-link action into the relevant deep
 * view. Worker / outbox / shard / fenced classes are server-gated and rendered
 * as honest "awaiting server support" rows — never fabricated counts.
 */
export function TriageView({ namespace, client }: TriageViewProps) {
  if (namespace === null) {
    return (
      <EmptyState
        description="Select a namespace to scope the triage view to its workflows."
        title="No namespace selected"
      />
    );
  }

  return <TriageViewInner client={client} />;
}

function TriageViewInner({ client }: { client?: ApiClient }) {
  const { incidents, isLoading, isError, error, refetch, gatedFeeds } = useIncidents({
    apiClient: client,
  });

  return (
    <section className="space-y-6" aria-label="Incidents">
      <header className="space-y-1">
        <h1 className="font-medium text-[var(--text-primary)] text-lg">Incidents</h1>
        <p className="text-[var(--text-muted)] text-sm">
          Ranked by urgency. Failures and timeouts first, then stuck-running workflows.
        </p>
      </header>

      <IncidentList
        error={error}
        incidents={incidents}
        isError={isError}
        isLoading={isLoading}
        onRetry={refetch}
      />

      <GatedFeedSection feeds={gatedFeeds} />
    </section>
  );
}

type IncidentListProps = {
  incidents: ReturnType<typeof useIncidents>['incidents'];
  isLoading: boolean;
  isError: boolean;
  error: unknown;
  onRetry: () => void;
};

function IncidentList({ incidents, isLoading, isError, error, onRetry }: IncidentListProps) {
  // No-silent-failure: a list-query error surfaces to a visible, retryable state
  // rather than an empty or stale board that hides the outage from the operator.
  if (isError) {
    return <ErrorState error={error} onRetry={onRetry} title="Could not load incident sources" />;
  }

  if (isLoading) {
    return <LoadingSkeleton label="Loading incidents" rows={4} />;
  }

  if (incidents.length === 0) {
    return (
      <EmptyState
        description="No failed, timed-out, or stuck workflows in this namespace."
        title="All clear"
      />
    );
  }

  return (
    <ul className="space-y-2">
      {incidents.map((incident) => (
        <li key={incident.id}>
          <IncidentCard incident={incident} />
        </li>
      ))}
    </ul>
  );
}

function GatedFeedSection({ feeds }: { feeds: readonly GatedFeed[] }) {
  if (feeds.length === 0) {
    return null;
  }

  return (
    <section aria-label="Awaiting server support" className="space-y-2">
      <h2 className="font-medium text-[var(--text-muted)] text-xs uppercase tracking-wide">
        Awaiting server support
      </h2>
      <ul className="space-y-1">
        {feeds.map((feed) => (
          <li
            className="flex items-center gap-3 border border-[var(--border-default)] border-dashed px-4 py-2"
            data-gated-kind={feed.kind}
            key={feed.kind}
          >
            <span aria-hidden className="font-mono text-[var(--text-muted)] text-sm">
              ◌
            </span>
            <span className="text-[var(--text-primary)] text-sm">{feed.label}</span>
            <span className="text-[var(--text-muted)] text-xs">— {feed.reason}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}
