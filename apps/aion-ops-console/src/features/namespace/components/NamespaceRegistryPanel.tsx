import { Boxes, Wifi, WifiOff } from 'lucide-react';

import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import {
  Badge,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui';
import type { AionSocketError, ConnectionStatus, NamespaceRecord } from '@/lib/api';
import { cn } from '@/lib/utils';

import { type NamespaceRegistryResult, useNamespaceRegistry } from '../hooks/useNamespaceRegistry';

/**
 * The live namespace registry panel (Control-Plane Phase 1, S8).
 *
 * Renders the REAL durable namespace set (loaded from `GET /namespaces/records`)
 * and appends/updates rows LIVE from the `NamespaceCreated` socket delta — no
 * refresh, no mock data. Columns: name, created, last seen, origin.
 */

const STATUS_LABEL: Record<ConnectionStatus, string> = {
  connected: 'Live',
  disconnected: 'Disconnected',
  reconnecting: 'Reconnecting',
};

const STATUS_STYLE: Record<ConnectionStatus, string> = {
  connected: 'border-emerald-500/40 bg-emerald-500/10 text-emerald-500',
  disconnected: 'border-destructive/40 bg-destructive/10 text-destructive',
  reconnecting: 'border-amber-500/40 bg-amber-500/10 text-amber-500',
};

/** Friendly text for the known origin labels; unknown labels pass through raw. */
const ORIGIN_LABEL: Record<string, string> = {
  worker_mint: 'Worker register',
  start_mint: 'Workflow start',
  explicit: 'Explicit create',
  inferred_from_state: 'Inferred from state',
};

export type NamespaceRegistryPanelProps = {
  /** Override the hook result (tests/storybook); defaults to the live hook. */
  registry?: NamespaceRegistryResult;
};

export function NamespaceRegistryPanel({ registry }: NamespaceRegistryPanelProps) {
  // The hook is the single live source; tests inject a result instead of the
  // socket + fetch so this component stays purely presentational under test.
  const live = useNamespaceRegistry();
  const { namespaces, loadState, loadError, status, socketError } = registry ?? live;

  return (
    <section className="flex flex-col gap-4" aria-label="Namespace registry">
      <header className="flex items-center justify-between gap-4">
        <div className="flex flex-col gap-1">
          <h1 className="font-semibold text-[var(--text-primary)] text-xl">Namespaces</h1>
          <p className="text-[var(--text-muted)] text-sm">
            The live durable namespace registry. New namespaces appear the moment a worker
            registers, a workflow starts, or one is created — no refresh.
          </p>
        </div>
        <ConnectionPill status={status} error={socketError} />
      </header>

      <NamespaceRegistryBody namespaces={namespaces} loadState={loadState} loadError={loadError} />
    </section>
  );
}

type NamespaceRegistryBodyProps = {
  namespaces: NamespaceRecord[];
  loadState: NamespaceRegistryResult['loadState'];
  loadError: Error | null;
};

function NamespaceRegistryBody({ namespaces, loadState, loadError }: NamespaceRegistryBodyProps) {
  if (loadState === 'loading' && namespaces.length === 0) {
    return <LoadingSkeleton />;
  }

  if (loadState === 'error' && namespaces.length === 0) {
    return (
      <ErrorState
        title="Could not load namespaces"
        message={loadError?.message ?? 'The namespace registry could not be read.'}
      />
    );
  }

  if (namespaces.length === 0) {
    return (
      <EmptyState
        icon={<Boxes className="size-6" aria-hidden="true" />}
        title="No namespaces yet"
        description="Register a worker or start a workflow and its namespace appears here live."
      />
    );
  }

  return <NamespaceTable namespaces={namespaces} />;
}

function NamespaceTable({ namespaces }: { namespaces: NamespaceRecord[] }) {
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Name</TableHead>
          <TableHead>Created</TableHead>
          <TableHead>Last seen</TableHead>
          <TableHead>Origin</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {namespaces.map((record) => (
          <TableRow key={record.name} data-namespace={record.name}>
            <TableCell className="font-medium text-[var(--text-primary)]">{record.name}</TableCell>
            <TableCell className="text-[var(--text-muted)]">
              <FormattedInstant value={record.createdAt} />
            </TableCell>
            <TableCell className="text-[var(--text-muted)]">
              <FormattedInstant value={record.lastSeen} />
            </TableCell>
            <TableCell>
              <Badge variant="outline" className="w-fit">
                {ORIGIN_LABEL[record.origin] ?? record.origin}
              </Badge>
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

/** Render an RFC 3339 instant as a localized, hover-for-exact timestamp. */
function FormattedInstant({ value }: { value: string }) {
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) {
    return <span>{value}</span>;
  }

  return (
    <time dateTime={value} title={value}>
      {parsed.toLocaleString()}
    </time>
  );
}

function ConnectionPill({
  status,
  error,
}: {
  status: ConnectionStatus;
  error: AionSocketError | null;
}) {
  const Icon = status === 'connected' ? Wifi : WifiOff;

  return (
    <div className="flex flex-col items-end gap-1" data-connection-status={status}>
      <Badge
        aria-label={`Namespace stream ${STATUS_LABEL[status].toLowerCase()}`}
        variant="outline"
        className={cn('w-fit gap-2', STATUS_STYLE[status])}
      >
        <Icon className="size-3.5" aria-hidden="true" />
        {STATUS_LABEL[status]}
      </Badge>
      {error === null ? null : (
        <p className="max-w-64 text-right text-[var(--text-muted)] text-xs">{error.message}</p>
      )}
    </div>
  );
}
