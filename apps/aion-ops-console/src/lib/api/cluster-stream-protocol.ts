import type { ClusterEvent, ClusterSnapshot, ClusterStreamError } from '@/types';

import type { AionSocketError, JsonRecord } from './websocket-types';

/** Server-frame discriminators pinned by `aion-proto` (`StreamedCluster*`). */
const CLUSTER_FRAME = {
  snapshot: 'cluster_snapshot',
  event: 'cluster_event',
} as const;

export type ParsedClusterFrame =
  | { kind: 'snapshot'; snapshot: ClusterSnapshot }
  | { kind: 'event'; event: ClusterEvent }
  | { kind: 'lagged'; lagged: ClusterStreamError };

export function parseClusterFrame(data: unknown): ParsedClusterFrame {
  const frame = parseJson(data);
  const lagged = readClusterLagged(frame);
  if (lagged !== null) {
    return { kind: 'lagged', lagged };
  }
  if (isSnapshotFrame(frame)) {
    return { kind: 'snapshot', snapshot: frame.snapshot };
  }
  if (isEventFrame(frame)) {
    return { kind: 'event', event: frame.event };
  }

  throw new Error('unrecognized cluster-stream frame shape');
}

export function clusterLaggedError(lagged: ClusterStreamError): AionSocketError {
  return {
    kind: 'frame-decode',
    subscriptionId: null,
    message: `The cluster stream fell behind and dropped ${lagged.skipped} update${
      lagged.skipped === 1 ? '' : 's'
    }; reconnecting to re-read the topology.`,
    cause: lagged,
  };
}

type SnapshotFrame = { snapshot: ClusterSnapshot };
type EventFrame = { event: ClusterEvent };

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function parseJson(data: unknown): unknown {
  if (typeof data === 'string') {
    return JSON.parse(data) as unknown;
  }
  if (data instanceof ArrayBuffer) {
    return JSON.parse(new TextDecoder().decode(data)) as unknown;
  }
  if (data instanceof Uint8Array) {
    return JSON.parse(new TextDecoder().decode(data)) as unknown;
  }
  return data;
}

function isSnapshotFrame(frame: unknown): frame is SnapshotFrame {
  if (!isRecord(frame) || frame.kind !== CLUSTER_FRAME.snapshot) {
    return false;
  }

  const snapshot = frame.snapshot;
  return (
    isRecord(snapshot) &&
    typeof snapshot.node === 'string' &&
    typeof snapshot.as_of_seq === 'number' &&
    Array.isArray(snapshot.peers) &&
    Array.isArray(snapshot.shards) &&
    Array.isArray(snapshot.workers)
  );
}

function isEventFrame(frame: unknown): frame is EventFrame {
  if (!isRecord(frame) || frame.kind !== CLUSTER_FRAME.event) {
    return false;
  }

  const event = frame.event;
  if (!isRecord(event) || typeof event.type !== 'string') {
    return false;
  }

  const meta = event.meta;
  return isRecord(meta) && typeof meta.cluster_seq === 'number';
}

function readClusterLagged(frame: unknown): ClusterStreamError | null {
  if (!isRecord(frame)) {
    return null;
  }

  const error = frame.error;
  if (!isRecord(error) || error.kind !== 'ClusterLagged' || typeof error.skipped !== 'number') {
    return null;
  }

  return { kind: 'ClusterLagged', skipped: error.skipped };
}
