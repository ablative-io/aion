import { actionsPath, failoverPath, namespacesPath, workflowDetailHref } from '@/app/routePaths';
import type { ApiClient } from '@/lib/api';
import type { Namespace, WorkflowFilter } from '@/types';

/** Entity kinds the palette can find. Each result deep-links to a real surface. */
export type PaletteEntityKind = 'workflow' | 'namespace' | 'package' | 'worker';

export type PaletteEntity = {
  kind: PaletteEntityKind;
  /** Stable identity for recents/dedup (kind-prefixed). */
  id: string;
  title: string;
  subtitle: string;
  /** Extra fuzzy-match terms beyond the title (type, status, node…). */
  keywords: string[];
  /** Router deep link. */
  href: string;
};

/** The real REST surface the palette searches — no mocks outside tests. */
export type PaletteEntityClient = Pick<
  ApiClient,
  'queryWorkflows' | 'listNamespaces' | 'listVersions' | 'requestClusterSnapshot'
>;

export type FetchPaletteEntitiesOptions = {
  namespace: Namespace | null;
  /**
   * Runtime-discovered deploy grant (`GET /whoami`). Packages and cluster
   * workers are deployment-scoped surfaces; without the grant the palette does
   * not issue requests the server would deny.
   */
  deployGranted: boolean;
};

export type PaletteEntityResult = {
  entities: PaletteEntity[];
  /** Per-source fetch failures, surfaced (never swallowed) as palette rows. */
  errors: string[];
};

const MATCH_ALL: WorkflowFilter = {
  workflow_type: null,
  status: null,
  started_after: null,
  started_before: null,
  parent: null,
};

const WORKFLOW_PAGE_LIMIT = 50;

/**
 * Fetch the searchable entity inventory from the live REST surface: workflows
 * (`POST /workflows/query`, selected namespace), namespaces (`GET /namespaces`),
 * deployed packages (`GET /deploy/versions`, deploy-gated) and cluster workers
 * (`POST /cluster/command` RequestClusterSnapshot, deploy-gated). Sources fail
 * independently: one denied/unreachable source becomes a visible error row while
 * the others still return.
 */
export async function fetchPaletteEntities(
  client: PaletteEntityClient,
  options: FetchPaletteEntitiesOptions
): Promise<PaletteEntityResult> {
  const sources: Array<{ label: string; load: () => Promise<PaletteEntity[]> }> = [
    {
      label: 'workflows',
      load: async () => {
        if (options.namespace === null) {
          return [];
        }
        const page = await client.queryWorkflows(
          MATCH_ALL,
          { limit: WORKFLOW_PAGE_LIMIT },
          { namespace: options.namespace }
        );
        return page.items.map((workflow) => ({
          kind: 'workflow' as const,
          id: `workflow:${workflow.workflow_id}`,
          title: workflow.workflow_id,
          subtitle: `${workflow.workflow_type} · ${workflow.status}`,
          keywords: [workflow.workflow_type, workflow.status],
          href: workflowDetailHref(workflow.workflow_id),
        }));
      },
    },
    {
      label: 'namespaces',
      load: async () => {
        const namespaces = await client.listNamespaces();
        return namespaces.map((name) => ({
          kind: 'namespace' as const,
          id: `namespace:${name}`,
          title: name,
          subtitle: 'namespace',
          keywords: [],
          href: namespacesPath,
        }));
      },
    },
    {
      label: 'packages',
      load: async () => {
        if (!options.deployGranted) {
          return [];
        }
        const versions = await client.listVersions();
        return versions.map((version) => ({
          kind: 'package' as const,
          id: `package:${version.workflowType}@${version.contentHash}`,
          title: version.workflowType,
          subtitle: `v${version.manifestVersion} · ${version.routeActive ? 'active' : 'inactive'}`,
          keywords: [version.manifestVersion, version.contentHash],
          // Deep-link to the start surface preloaded with this workflow type.
          href: `${actionsPath}?workflow_type=${encodeURIComponent(version.workflowType)}`,
        }));
      },
    },
    {
      label: 'workers',
      load: async () => {
        if (!options.deployGranted) {
          return [];
        }
        const snapshot = await client.requestClusterSnapshot();
        return snapshot.workers.map((worker) => ({
          kind: 'worker' as const,
          id: `worker:${worker.worker_id}`,
          title: worker.worker_id,
          subtitle: `${worker.task_queue}${worker.node === null ? '' : ` @ ${worker.node}`}`,
          keywords: [
            worker.task_queue,
            ...worker.namespaces,
            ...(worker.node === null ? [] : [worker.node]),
          ],
          href: failoverPath,
        }));
      },
    },
  ];

  const settled = await Promise.allSettled(sources.map((source) => source.load()));
  const entities: PaletteEntity[] = [];
  const errors: string[] = [];

  settled.forEach((outcome, index) => {
    const label = sources[index]?.label ?? 'unknown';
    if (outcome.status === 'fulfilled') {
      entities.push(...outcome.value);
    } else {
      errors.push(`${label}: ${describeError(outcome.reason)}`);
    }
  });

  return { entities, errors };
}

function describeError(reason: unknown): string {
  if (reason instanceof Error) {
    return reason.message;
  }
  return String(reason);
}
