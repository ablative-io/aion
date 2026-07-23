import { useQuery } from '@tanstack/react-query';
import { ChevronRight } from 'lucide-react';
import { useMemo, useState } from 'react';

import { Button } from '@/components/ui';
import { useNamespace } from '@/features/namespace';
import type { ApiClient } from '@/lib/api';
import { createConfiguredApiClient } from '@/lib/config';
import { cn } from '@/lib/utils';
import type { Event } from '@/types';

import {
  decodePayload,
  findPayloadElision,
  formatPayloadKilobytes,
  isPayloadElision,
  payloadSummary,
} from '../lib/timeline';

export type PayloadEventClient = Pick<ApiClient, 'getEvent'>;

type PayloadViewProps = {
  payload: unknown;
  event?: Event | undefined;
  label?: string;
  apiClient?: PayloadEventClient | undefined;
};

type ValuePath = readonly (number | string)[];

export function workflowEventQueryKey(workflowId: string, seq: number) {
  return ['workflow-event', workflowId, seq] as const;
}

export function fullPayloadEventQueryOptions(
  client: PayloadEventClient,
  event: Event,
  namespace: string
) {
  const workflowId = event.data.envelope.workflow_id;
  const seq = event.data.envelope.seq;
  return {
    queryKey: workflowEventQueryKey(workflowId, seq),
    queryFn: () => client.getEvent(workflowId, seq, { namespace }),
    staleTime: Number.POSITIVE_INFINITY,
  } as const;
}

function PayloadView({ payload, event, label = 'Payload', apiClient }: PayloadViewProps) {
  const elision = findPayloadElision(payload);
  if (elision !== null) {
    return (
      <ElidedPayloadView apiClient={apiClient} event={event} label={label} payload={payload} />
    );
  }
  return <ExpandedPayload initialOpen={false} label={label} payload={payload} />;
}

function ElidedPayloadView({
  payload,
  event,
  label,
  apiClient,
}: Required<Pick<PayloadViewProps, 'label' | 'payload'>> &
  Pick<PayloadViewProps, 'apiClient' | 'event'>) {
  const { selectedNamespace } = useNamespace();
  const client = useMemo<PayloadEventClient>(
    () => apiClient ?? createConfiguredApiClient({ namespace: selectedNamespace }),
    [apiClient, selectedNamespace]
  );
  const workflowId = event?.data.envelope.workflow_id ?? '';
  const seq = event?.data.envelope.seq ?? -1;
  const payloadPath = useMemo(
    () => (event === undefined ? null : findValuePath(event.data, payload)),
    [event, payload]
  );
  const query = useQuery({
    ...(event === undefined || selectedNamespace === null
      ? {
          queryKey: workflowEventQueryKey(workflowId, seq),
          queryFn: () =>
            Promise.reject(
              new Error('Cannot load an elided payload without its workflow event and namespace.')
            ),
          staleTime: Number.POSITIVE_INFINITY,
        }
      : fullPayloadEventQueryOptions(client, event, selectedNamespace)),
    enabled: false,
  });
  const loadedPayload = useMemo(() => {
    if (query.data === undefined) {
      return undefined;
    }
    if (payloadPath !== null) {
      return valueAtPath(query.data.data, payloadPath);
    }
    return hydrateElisions(payload, query.data.data);
  }, [payload, payloadPath, query.data]);
  const elision = findPayloadElision(payload);
  const sizeLabel = formatPayloadKilobytes(elision?.size_bytes ?? 0);

  if (loadedPayload !== undefined) {
    return <ExpandedPayload initialOpen label={label} payload={loadedPayload} />;
  }

  return (
    <div className="mt-3 rounded-lg border border-border bg-surface-base px-3 py-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-medium text-secondary-foreground text-xs uppercase tracking-wide">
          {label}
        </span>
        <Button
          className="h-7 px-2 font-mono text-xs"
          disabled={query.isFetching || event === undefined}
          onClick={() => void query.refetch()}
          type="button"
          variant="outline"
        >
          {query.isFetching ? `${sizeLabel} — loading` : `${sizeLabel} — load`}
        </Button>
      </div>
      {query.error === null ? null : (
        <p className="mt-2 text-danger text-xs" role="alert">
          {query.error instanceof Error ? query.error.message : 'Could not load the full payload.'}
        </p>
      )}
    </div>
  );
}

function ExpandedPayload({
  payload,
  label,
  initialOpen,
}: {
  payload: unknown;
  label: string;
  initialOpen: boolean;
}) {
  const [open, setOpen] = useState(initialOpen);
  const summary = useMemo(() => payloadSummary(payload), [payload]);
  const formattedPayload = useMemo(() => (open ? stringifyPayload(payload) : ''), [open, payload]);

  return (
    <div className="mt-3 rounded-lg border border-border bg-surface-base">
      <Button
        aria-expanded={open}
        className="h-auto w-full justify-start rounded-lg px-3 py-2 text-left"
        onClick={() => setOpen((current) => !current)}
        type="button"
        variant="ghost"
      >
        <ChevronRight className={cn('size-4 transition-transform', open && 'rotate-90')} />
        <span className="font-medium text-secondary-foreground text-xs uppercase tracking-wide">
          {label}
        </span>
        <span className="truncate text-muted-foreground text-sm">{summary}</span>
      </Button>
      {open ? (
        <pre className="max-h-72 overflow-auto border-border border-t p-3 font-mono text-secondary-foreground text-xs leading-relaxed">
          {formattedPayload}
        </pre>
      ) : null}
    </div>
  );
}

function findValuePath(root: unknown, target: unknown, path: ValuePath = []): ValuePath | null {
  if (root === target) {
    return path;
  }
  if (Array.isArray(root)) {
    for (const [index, value] of root.entries()) {
      const found = findValuePath(value, target, [...path, index]);
      if (found !== null) {
        return found;
      }
    }
    return null;
  }
  if (typeof root !== 'object' || root === null) {
    return null;
  }
  for (const [key, value] of Object.entries(root)) {
    const found = findValuePath(value, target, [...path, key]);
    if (found !== null) {
      return found;
    }
  }
  return null;
}

function valueAtPath(root: unknown, path: ValuePath): unknown {
  let value = root;
  for (const key of path) {
    if (typeof value !== 'object' || value === null || !(key in value)) {
      throw new Error('Full event did not contain the projected payload path.');
    }
    value = (value as Record<number | string, unknown>)[key];
  }
  return value;
}

/** Hydrate a synthesized projection (for example an activity failure list) by byte order. */
function hydrateElisions(projected: unknown, fullEventData: unknown): unknown {
  const fullByteFields = collectFullPayloadBytes(fullEventData);
  let index = 0;
  const visit = (value: unknown): unknown => {
    if (isPayloadElision(value)) {
      const replacement = fullByteFields[index];
      index += 1;
      return replacement ?? value;
    }
    if (Array.isArray(value)) {
      return value.map(visit);
    }
    if (typeof value !== 'object' || value === null) {
      return value;
    }
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, visit(item)]));
  };
  return visit(projected);
}

function collectFullPayloadBytes(value: unknown): number[][] {
  if (Array.isArray(value)) {
    return value.flatMap(collectFullPayloadBytes);
  }
  if (typeof value !== 'object' || value === null) {
    return [];
  }
  const record = value as Record<string, unknown>;
  const ownBytes =
    Array.isArray(record.bytes) && record.bytes.every((byte) => typeof byte === 'number')
      ? [record.bytes as number[]]
      : [];
  return [...ownBytes, ...Object.values(record).flatMap(collectFullPayloadBytes)];
}

function stringifyPayload(payload: unknown): string {
  const decodedPayload = decodePayload(payload);

  try {
    return JSON.stringify(decodedPayload, null, 2) ?? String(decodedPayload);
  } catch {
    return String(decodedPayload);
  }
}

export { PayloadView, stringifyPayload };
