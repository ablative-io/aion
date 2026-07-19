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
type ClusterEventValidator = (event: JsonRecord) => boolean;

const CLUSTER_EVENT_VALIDATORS = {
  PeerAdded: (event) =>
    hasStringFields(event, ['peer_name']) && isNullableString(event.forward_addr),
  PeerConnected: (event) =>
    hasStringFields(event, ['peer_name']) && isNullableString(event.forward_addr),
  PeerDisconnected: (event) =>
    hasStringFields(event, ['peer_name']) &&
    isSafeUnsignedInteger(event.consecutive_down) &&
    typeof event.confirmed === 'boolean',
  ShardAdopted: (event) =>
    isSafeUnsignedIntegerArray(event.shards) && hasStringFields(event, ['from_peer', 'adopted_by']),
  ShardAdoptionFailed: (event) =>
    isSafeUnsignedIntegerArray(event.shards) && hasStringFields(event, ['from_peer', 'error']),
  ShardAdoptionSkipped: (event) =>
    isSafeUnsignedIntegerArray(event.shards) && hasStringFields(event, ['from_peer', 'held_by']),
  WorkerConnected: (event) =>
    hasStringFields(event, ['worker_id', 'task_queue']) &&
    isStringArray(event.namespaces) &&
    isWorkerTransport(event.transport) &&
    isNullableString(event.node),
  WorkerDisconnected: (event) =>
    hasStringFields(event, ['worker_id']) &&
    isStringArray(event.namespaces) &&
    isWorkerDeathReason(event.reason),
  SupervisorStarted: (event) => hasStringFields(event, ['node']),
  SupervisorStopped: (event) => hasStringFields(event, ['node']),
  NamespaceCreated: (event) => hasStringFields(event, ['name', 'created_at', 'origin']),
  NamespacePlacementChanged: (event) =>
    hasStringFields(event, ['name']) && isNamespacePlacement(event.placement),
  NamespaceQuotaState: (event) =>
    hasStringFields(event, ['namespace']) &&
    isSafeUnsignedInteger(event.in_flight) &&
    isSafeUnsignedInteger(event.ceiling),
} satisfies { [Type in ClusterEvent['type']]: ClusterEventValidator };

type ClusterEventType = keyof typeof CLUSTER_EVENT_VALIDATORS;

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
    isSafeUnsignedInteger(snapshot.as_of_seq) &&
    Array.isArray(snapshot.peers) &&
    snapshot.peers.every(isClusterPeer) &&
    Array.isArray(snapshot.shards) &&
    snapshot.shards.every(isClusterShard) &&
    Array.isArray(snapshot.workers) &&
    snapshot.workers.every(isClusterWorker)
  );
}

function isEventFrame(frame: unknown): frame is EventFrame {
  if (!isRecord(frame) || frame.kind !== CLUSTER_FRAME.event) {
    return false;
  }

  const event = frame.event;
  if (!isRecord(event) || !isClusterEventMeta(event.meta) || !isClusterEventType(event.type)) {
    return false;
  }

  return CLUSTER_EVENT_VALIDATORS[event.type](event);
}

function isClusterEventType(value: unknown): value is ClusterEventType {
  return typeof value === 'string' && Object.hasOwn(CLUSTER_EVENT_VALIDATORS, value);
}

function isClusterEventMeta(value: unknown): boolean {
  return (
    isRecord(value) &&
    isSafeUnsignedInteger(value.cluster_seq) &&
    typeof value.observed_at === 'string'
  );
}

function isClusterPeer(value: unknown): boolean {
  return (
    isRecord(value) &&
    hasStringFields(value, ['peer_name']) &&
    isNullableString(value.forward_addr) &&
    typeof value.connected === 'boolean' &&
    isSafeUnsignedInteger(value.consecutive_down)
  );
}

function isClusterShard(value: unknown): boolean {
  return (
    isRecord(value) &&
    isSafeUnsignedInteger(value.shard) &&
    typeof value.owner === 'string' &&
    isSafeUnsignedInteger(value.epoch)
  );
}

function isClusterWorker(value: unknown): boolean {
  return (
    isRecord(value) &&
    hasStringFields(value, ['worker_id', 'task_queue']) &&
    isStringArray(value.namespaces) &&
    isWorkerTransport(value.transport) &&
    isNullableString(value.node)
  );
}

function isWorkerTransport(value: unknown): boolean {
  return isRecord(value) && (value.transport === 'Grpc' || value.transport === 'Liminal');
}

function isWorkerDeathReason(value: unknown): boolean {
  return (
    isRecord(value) &&
    (value.reason === 'Disconnect' || value.reason === 'Timeout' || value.reason === 'Deregistered')
  );
}

function isNamespacePlacement(value: unknown): boolean {
  return isRecord(value) && typeof value.kind === 'string' && isStringArray(value.nodes);
}

function hasStringFields(record: JsonRecord, fields: readonly string[]): boolean {
  return fields.every((field) => typeof record[field] === 'string');
}

function isNullableString(value: unknown): boolean {
  return value === null || typeof value === 'string';
}

function isStringArray(value: unknown): boolean {
  return Array.isArray(value) && value.every((entry) => typeof entry === 'string');
}

function isSafeUnsignedInteger(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function isSafeUnsignedIntegerArray(value: unknown): boolean {
  return Array.isArray(value) && value.every(isSafeUnsignedInteger);
}

function readClusterLagged(frame: unknown): ClusterStreamError | null {
  if (!isRecord(frame)) {
    return null;
  }

  const error = frame.error;
  if (!isRecord(error) || error.kind !== 'ClusterLagged' || !isSafeUnsignedInteger(error.skipped)) {
    return null;
  }

  return { kind: 'ClusterLagged', skipped: error.skipped };
}
