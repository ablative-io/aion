import { useMemo } from 'react';
import { Link } from 'react-router';

import { assistantSessionHref } from '@/app/routePaths';
import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import { StatusBadge } from '@/components/StatusBadge';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui';
import { useLiveListUpdates, useWorkflowQuery } from '@/features/workflow-list';
import type { ApiClient } from '@/lib/api';
import type { Namespace, WorkflowSummary } from '@/types';

import { assistantSessionsFilter } from '../lib/contract';
import { sortSessionsNewestFirst } from '../lib/mode';

const SESSION_PAGE_LIMIT = 50;

export type AssistantSessionListProps = {
  namespace: Namespace | null;
  /** Injected client (tests); defaults to the configured live client. */
  client?: Pick<ApiClient, 'queryWorkflows'> | undefined;
};

/**
 * The assistant session list: `POST /workflows/list` filtered to the assistant
 * workflow type in the selected namespace, newest first, live-patched through
 * the same socket subscription the workflow list uses (rows flip status and new
 * sessions appear without a refetch). Each row deep-links to its session.
 */
export function AssistantSessionList({ namespace, client }: AssistantSessionListProps) {
  const filter = useMemo(assistantSessionsFilter, []);
  const page = useMemo(() => ({ limit: SESSION_PAGE_LIMIT }), []);
  const query = useWorkflowQuery({ apiClient: client, filter, page });
  useLiveListUpdates({ filter, page, isFirstPage: true });

  if (namespace === null) {
    return (
      <EmptyState
        description="Select a namespace to list assistant sessions."
        title="No namespace selected"
      />
    );
  }
  if (query.isPending) {
    return <LoadingSkeleton label="Loading assistant sessions" rows={4} />;
  }
  if (query.isError) {
    return (
      <ErrorState
        error={query.error}
        onRetry={() => void query.refetch()}
        title="Could not load assistant sessions"
      />
    );
  }

  return <AssistantSessionRows sessions={query.data?.items ?? []} />;
}

/** Pure rows body (tested via static markup). Sorts newest first. */
export function AssistantSessionRows({ sessions }: { sessions: readonly WorkflowSummary[] }) {
  if (sessions.length === 0) {
    return (
      <EmptyState
        description="Start one above — the session appears here the moment the server confirms it."
        title="No assistant sessions yet"
      />
    );
  }

  return (
    <Table data-testid="assistant-session-list">
      <TableHeader>
        <TableRow>
          <TableHead>Session</TableHead>
          <TableHead>Status</TableHead>
          <TableHead>Started</TableHead>
          <TableHead>Ended</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {sortSessionsNewestFirst(sessions).map((session) => (
          <TableRow key={session.workflow_id}>
            <TableCell className="font-mono text-xs">
              <Link
                className="text-primary underline-offset-4 hover:underline"
                to={assistantSessionHref(session.workflow_id)}
              >
                {session.workflow_id}
              </Link>
            </TableCell>
            <TableCell>
              <StatusBadge status={session.status} />
            </TableCell>
            <TableCell>{session.started_at}</TableCell>
            <TableCell>{session.ended_at ?? '—'}</TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}
