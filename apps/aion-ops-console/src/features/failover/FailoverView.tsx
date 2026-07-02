import { useEffect, useMemo, useRef, useState } from 'react';

import { EmptyState } from '@/components/EmptyState';
import { FailoverErrorBoundary } from '@/components/FailoverErrorBoundary';
import { eventSequence } from '@/features/workflow-detail/lib/timeline';
import type { Event, Namespace } from '@/types';

import { AdoptionStrip } from './components/AdoptionStrip';
import { ClusterProvenance } from './components/ClusterProvenance';
import { EventLog, type SyntheticRow } from './components/EventLog';
import { ExactlyOnceCounter } from './components/ExactlyOnceCounter';
import { FanOutBar } from './components/FanOutBar';
import { HeaderBar } from './components/HeaderBar';
import { NodeGrid } from './components/NodeGrid';
import { useAdoptionMoment } from './hooks/useAdoptionMoment';
import { useClusterStream } from './hooks/useClusterStream';
import { useDiscoveredWorkflowId } from './hooks/useDiscoveredWorkflowId';
import { useFanOutProgress } from './hooks/useFanOutProgress';
import { useNodeLiveness } from './hooks/useNodeLiveness';
import { useNodeMetrics } from './hooks/useNodeMetrics';
import { buildClusterConfig, type ClusterConfig } from './lib/clusterConfig';
import { hasOverCount } from './lib/exactlyOnce';

/**
 * Top-level failover view (DEMO-VIEW-PLAN §1). Composes the sections, mounts the
 * error boundary so a bad frame can never white-screen, and gates on the namespace
 * being selected. Every signal flows from the existing data layer; this component
 * only arranges shape + position and synthesizes the health/adoption log rows.
 */

const FAN_OUT_LABEL = 'collect_four';
const SEED_ARITY = 4;

export type FailoverViewProps = {
  namespace: Namespace | null;
  /** Override for tests; defaults to the live URL-derived cluster config. */
  config?: ClusterConfig | undefined;
};

export function FailoverView({ namespace, config }: FailoverViewProps) {
  if (namespace === null) {
    return (
      <EmptyState
        message="Select a namespace to scope the failover view to the demo cluster."
        title="No namespace selected"
      />
    );
  }

  return (
    <FailoverErrorBoundary>
      <FailoverViewInner config={config} namespace={namespace} />
    </FailoverErrorBoundary>
  );
}

function FailoverViewInner({
  namespace,
  config,
}: {
  namespace: Namespace;
  config?: ClusterConfig | undefined;
}) {
  const cluster = useMemo<ClusterConfig>(
    () =>
      config ??
      buildClusterConfig({
        search: typeof window === 'undefined' ? '' : window.location.search,
        namespace,
      }),
    [config, namespace]
  );

  // WS3: liveness + worker counts now flow from the LIVE cluster stream (a
  // dedicated `/events/stream` cluster socket), not `/health/live` + `/metrics`
  // polling. The stream is the single push source for topology.
  const clusterStream = useClusterStream();
  const { liveness, activeTarget, activeBaseUrl, failedOver } = useNodeLiveness({
    nodes: cluster.nodes,
    preferredTargetIndex: cluster.ownerIndex,
    topology: clusterStream.topology,
    primed: clusterStream.primed,
  });
  const metrics = useNodeMetrics({
    nodes: cluster.nodes,
    topology: clusterStream.topology,
    primed: clusterStream.primed,
  });

  // When no `?workflow=` is seeded, discover the most recent workflow so the
  // exactly-once counter back-fills from describe even without live WS events.
  const { workflowId, error: discoveryError } = useDiscoveredWorkflowId({
    namespace,
    seeded: cluster.workflowId,
    baseUrl: activeBaseUrl ?? undefined,
  });

  const progress = useFanOutProgress({
    namespace,
    workflowId,
    baseUrl: activeBaseUrl ?? undefined,
    seedArity: SEED_ARITY,
  });

  const ownerLiveness = liveness.find((entry) => entry.nodeIndex === cluster.ownerIndex);
  const moment = useAdoptionMoment({
    ownerLiveness,
    completedCount: progress.completedCount,
  });

  // The active shard owner: the seeded owner until it goes dark, then the survivor
  // the view has failed its reads over to. This is what MOVES the owner marker.
  const activeOwnerIndex = useMemo(() => {
    if (ownerLiveness?.state === 'dark' && activeTarget !== null) {
      return activeTarget.index;
    }
    return cluster.ownerIndex;
  }, [ownerLiveness?.state, activeTarget, cluster.ownerIndex]);

  const preKill = ownerLiveness?.state !== 'dark';

  const oldOwnerLabel =
    cluster.nodes.find((node) => node.index === cluster.ownerIndex)?.label ?? 'node 0';
  const newOwnerLabel =
    activeOwnerIndex === cluster.ownerIndex
      ? null
      : (cluster.nodes.find((node) => node.index === activeOwnerIndex)?.label ?? null);

  const synthetic = useSyntheticRows({
    events: progress.events,
    ownerDark: ownerLiveness?.state === 'dark',
    ownerLabel: oldOwnerLabel,
    adoptionAdopted: moment.adopted,
    adoptionInstant: moment.instant,
    adopterLabel: newOwnerLabel,
  });

  const overCount =
    workflowId !== null && progress.arity !== null
      ? hasOverCount(progress.events, progress.arity)
      : false;

  const surfacedError = progress.error ?? discoveryError;

  return (
    <div className="flex flex-col gap-4">
      <HeaderBar
        activeNodeLabel={activeTarget?.label ?? null}
        failedOver={failedOver}
        namespace={namespace}
        status={
          progress.stale ? (progress.error !== null ? 'disconnected' : 'reconnecting') : 'connected'
        }
      />

      <NodeGrid
        activeOwnerIndex={activeOwnerIndex}
        liveness={liveness}
        metrics={metrics}
        nodes={cluster.nodes}
        preKill={preKill}
      />

      <ClusterProvenance
        error={clusterStream.error}
        failedOver={failedOver}
        lastObservedAt={clusterStream.lastObservedAt}
        lastSeq={clusterStream.lastSeq}
        primed={clusterStream.primed}
        readingNodeLabel={activeTarget?.label ?? null}
        selfNode={clusterStream.topology.selfNode}
        status={clusterStream.status}
      />

      <AdoptionStrip moment={moment} newOwnerLabel={newOwnerLabel} oldOwnerLabel={oldOwnerLabel} />

      <FanOutBar
        arity={progress.arity}
        completed={progress.completedCount}
        label={FAN_OUT_LABEL}
        stale={progress.stale}
      />

      <ExactlyOnceCounter
        arity={progress.arity}
        completed={progress.completedCount}
        overCount={overCount}
        stale={progress.stale}
      />

      {surfacedError !== null ? (
        <p className="rounded-md border border-destructive/40 bg-destructive/5 px-4 py-2 text-destructive text-sm">
          back-fill error: {surfacedError}
        </p>
      ) : null}

      <EventLog events={progress.events} startedAt={moment.instant} synthetic={synthetic} />
    </div>
  );
}

type SyntheticRowsInput = {
  events: readonly Event[];
  ownerDark: boolean;
  ownerLabel: string;
  adoptionAdopted: boolean;
  adoptionInstant: number | null;
  adopterLabel: string | null;
};

/**
 * Synthesize the health-dark and adoption log rows once each, pinned just past the
 * latest wire sequence at the moment they are observed so they interleave at the
 * right place in the seq-ordered log. Recorded once via refs so re-renders do not
 * fabricate duplicate rows.
 */
function useSyntheticRows({
  events,
  ownerDark,
  ownerLabel,
  adoptionAdopted,
  adoptionInstant,
  adopterLabel,
}: SyntheticRowsInput): SyntheticRow[] {
  const [rows, setRows] = useState<SyntheticRow[]>([]);
  const darkRecorded = useRef(false);
  const adoptionRecorded = useRef(false);

  const maxSeq = useMemo(() => {
    let max = 0;
    for (const event of events) {
      const seq = eventSequence(event);
      if (seq > max) {
        max = seq;
      }
    }
    return max;
  }, [events]);

  useEffect(() => {
    if (ownerDark && !darkRecorded.current) {
      darkRecorded.current = true;
      setRows((previous) => [
        ...previous,
        {
          id: 'health:owner-dark',
          sequence: maxSeq + 0.4,
          observedAt: Date.now(),
          kind: 'health',
          glyph: '⚠',
          label: `${ownerLabel} went dark`,
          detail: '(cluster: peer down)',
        },
      ]);
    }
  }, [ownerDark, ownerLabel, maxSeq]);

  useEffect(() => {
    if (adoptionAdopted && !adoptionRecorded.current) {
      adoptionRecorded.current = true;
      setRows((previous) => [
        ...previous,
        {
          id: 'adoption:moment',
          sequence: maxSeq + 0.6,
          observedAt: adoptionInstant ?? Date.now(),
          kind: 'adoption',
          glyph: '★',
          label: 'shard 0 adopted',
          detail: `${adopterLabel ?? 'survivor'} (SS-5b)`,
        },
      ]);
    }
  }, [adoptionAdopted, adoptionInstant, adopterLabel, maxSeq]);

  return rows;
}
