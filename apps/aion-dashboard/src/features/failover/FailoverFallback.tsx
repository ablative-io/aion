import { useQueries, useQuery } from '@tanstack/react-query';
import { useMemo } from 'react';

import type { ApiClient } from '@/lib/api';
import { createConfiguredApiClient } from '@/lib/config';
import type { Event, Namespace } from '@/types';

import { buildClusterConfig, type ClusterConfig } from './lib/clusterConfig';
import { tallyExactlyOnce } from './lib/exactlyOnce';

/**
 * Tier-1 BULLETPROOF FALLBACK (DEMO-VIEW-PLAN §7). The floor that cannot fail to
 * render: pure HTTP polling, no WebSocket dependency, no generated-variant timeline.
 *
 * It reads only three signals — `/health/live` per node (liveness), a describe-poll
 * of the workflow history (the exactly-once tally), and the tail of that same
 * history (the event log) — and shares `clusterConfig.ts` + `exactlyOnce.ts` with
 * the Tier-2 view so the numbers ALWAYS agree. Polling describe and deduping by
 * ordinal can never double-count, so the headline number is structurally honest
 * even if every other surface is down. WS is an enhancement, never a dependency.
 */

const LIVENESS_POLL_MS = 1000;
const TALLY_POLL_MS = 1000;
const HEALTH_PATH = '/health/live';
const REQUEST_TIMEOUT_MS = 800;
const SEED_ARITY = 4;
const EVENT_TAIL = 20;

type LivenessState = 'live' | 'dark' | 'unknown';

export type FailoverFallbackProps = {
  namespace: Namespace | null;
  config?: ClusterConfig;
  /** Override for tests; defaults to a fresh ApiClient per active base URL. */
  apiClient?: Pick<ApiClient, 'getHistory'>;
  fetchImpl?: typeof fetch;
};

async function probe(baseUrl: string, fetchImpl: typeof fetch): Promise<LivenessState> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);

  try {
    const response = await fetchImpl(`${baseUrl}${HEALTH_PATH}`, { signal: controller.signal });
    return response.ok ? 'live' : 'dark';
  } catch {
    return 'dark';
  } finally {
    clearTimeout(timeout);
  }
}

export function FailoverFallback({
  namespace,
  config,
  apiClient,
  fetchImpl = fetch,
}: FailoverFallbackProps) {
  const cluster = useMemo<ClusterConfig>(
    () =>
      config ??
      buildClusterConfig({
        search: typeof window === 'undefined' ? '' : window.location.search,
        ...(namespace !== null && { namespace }),
      }),
    [config, namespace]
  );

  const livenessResults = useQueries({
    queries: cluster.nodes.map((node) => ({
      queryKey: ['failover-fallback', 'liveness', node.index, node.baseUrl] as const,
      queryFn: () => probe(node.baseUrl, fetchImpl),
      refetchInterval: LIVENESS_POLL_MS,
      retry: false,
    })),
  });

  const liveness = cluster.nodes.map((node, index) => ({
    node,
    state: (livenessResults[index]?.data ?? 'unknown') as LivenessState,
  }));

  // First live node is the read target for the tally; reads survive a node kill.
  const target = liveness.find((entry) => entry.state === 'live')?.node ?? cluster.nodes[0] ?? null;

  const { workflowId } = cluster;
  const historyQuery = useQuery<Event[], Error>({
    queryKey: ['failover-fallback', 'history', workflowId, target?.baseUrl] as const,
    enabled: workflowId !== null && target !== null,
    refetchInterval: TALLY_POLL_MS,
    retry: false,
    queryFn: () => {
      if (workflowId === null) {
        throw new Error('history poll requires a seeded workflow id');
      }
      const client =
        apiClient ??
        createConfiguredApiClient({
          namespace: cluster.namespace,
          ...(target && { baseUrl: target.baseUrl }),
        });
      return client.getHistory(workflowId, { namespace: cluster.namespace });
    },
  });

  const events = historyQuery.data ?? [];
  const tally = tallyExactlyOnce(events);
  const done = Math.min(tally.count, SEED_ARITY);

  return (
    <div className="mx-auto flex max-w-2xl flex-col gap-6 p-6 font-mono text-[var(--text-primary)]">
      <header className="flex flex-col gap-1">
        <p className="text-[0.7rem] text-[var(--text-muted)] uppercase tracking-[0.3em]">
          AION · FAILOVER · FALLBACK
        </p>
        <p className="text-[var(--text-muted)] text-xs">
          pure-poll floor · ns: {cluster.namespace} · WS not required
        </p>
      </header>

      <section className="flex flex-col gap-2">
        <span className="text-[var(--text-muted)] text-xs uppercase tracking-wide">nodes</span>
        {liveness.map(({ node, state }) => (
          <div className="flex items-center gap-3 text-sm" key={node.index}>
            <span
              aria-hidden="true"
              className="inline-block size-2.5 rounded-full"
              style={{
                backgroundColor:
                  state === 'live'
                    ? 'var(--accent-cyan)'
                    : state === 'dark'
                      ? 'var(--destructive)'
                      : 'transparent',
                border: state === 'unknown' ? '1px solid var(--text-muted)' : 'none',
              }}
            />
            <span className="w-20">{node.label}</span>
            <span
              style={{
                color:
                  state === 'live'
                    ? 'var(--accent-cyan)'
                    : state === 'dark'
                      ? 'var(--destructive)'
                      : 'var(--text-muted)',
              }}
            >
              {state.toUpperCase()}
            </span>
          </div>
        ))}
      </section>

      <section className="flex flex-col items-center gap-1 rounded-md border border-[var(--border-default)] py-8">
        <span className="text-[var(--text-muted)] text-xs uppercase tracking-[0.3em]">
          exactly once
        </span>
        <span className="font-bold text-7xl tabular-nums">
          {done} / {SEED_ARITY}
        </span>
        {cluster.workflowId === null ? (
          <span className="text-[var(--text-muted)] text-xs">
            no ?workflow= seeded — pass the workload id to count
          </span>
        ) : null}
        {historyQuery.isError ? (
          <span className="text-[var(--destructive)] text-xs">
            describe poll failed: {historyQuery.error.message}
          </span>
        ) : null}
      </section>

      <section className="flex flex-col gap-1">
        <span className="text-[var(--text-muted)] text-xs uppercase tracking-wide">event tail</span>
        {events.length === 0 ? (
          <span className="text-[var(--text-muted)] text-sm">no events</span>
        ) : (
          <ol className="flex flex-col gap-0.5 text-xs">
            {events.slice(-EVENT_TAIL).map((event) => (
              <li
                className="text-[var(--text-secondary)]"
                key={`${event.data.envelope.seq}:${event.type}`}
              >
                #{event.data.envelope.seq} {event.type}
              </li>
            ))}
          </ol>
        )}
      </section>
    </div>
  );
}
