import { expect, test } from 'bun:test';

import type { StorageLike } from '@/lib/keybindings';
import type { ClusterSnapshot, Namespace } from '@/types';

import { fetchPaletteEntities, type PaletteEntityClient } from '../lib/entities';
import { buildPaletteGroups } from '../lib/groups';
import { cycleMode, type PaletteMode } from '../lib/modes';
import {
  loadRecents,
  PALETTE_RECENTS_STORAGE_KEY,
  pushRecent,
  type RecentSelection,
} from '../lib/recents';
import { paletteVerbs } from '../lib/verbs';

// --- mode cycling ---

test('Tab cycles everything → workflows → actions → everything', () => {
  const seen: PaletteMode[] = ['everything'];
  seen.push(cycleMode(seen[0] ?? 'everything'));
  seen.push(cycleMode(seen[1] ?? 'everything'));
  seen.push(cycleMode(seen[2] ?? 'everything'));

  expect(seen).toEqual(['everything', 'workflows', 'actions', 'everything']);
});

// --- entity fetch over the (mocked-in-test-only) API client ---

const WORKFLOW = {
  workflow_id: 'wf-123',
  workflow_type: 'EmailDigest',
  status: 'Running' as const,
  started_at: '2026-07-01T00:00:00Z',
  ended_at: null,
  parent: null,
};

const ASSISTANT_SESSION = {
  workflow_id: 'wf-assist-1',
  workflow_type: 'assistant',
  status: 'Running' as const,
  started_at: '2026-07-02T00:00:00Z',
  ended_at: null,
  parent: null,
};

const SNAPSHOT: ClusterSnapshot = {
  node: 'node-a',
  as_of_seq: 1,
  peers: [],
  shards: [],
  workers: [
    {
      worker_id: 'worker-7',
      namespaces: ['default'],
      task_queue: 'default',
      transport: { transport: 'Grpc' },
      node: 'node-a',
    },
  ],
};

function mockClient(overrides: Partial<PaletteEntityClient> = {}): PaletteEntityClient {
  return {
    // The workflows and assistant-sessions sources share this endpoint; the
    // assistant source narrows by workflow_type, so the mock honors the filter.
    queryWorkflows: async (filter) => ({
      items: filter.workflow_type === 'assistant' ? [ASSISTANT_SESSION] : [WORKFLOW],
      nextCursor: null,
      hasMore: false,
    }),
    listNamespaces: async () => ['default', 'orders'] as Namespace[],
    listVersions: async () => [
      {
        workflowType: 'EmailDigest',
        contentHash: 'abc123',
        deployedEntryModule: 'digest.mjs',
        entryFunction: 'run',
        manifestVersion: '1.2.0',
        loadedAt: '2026-07-01T00:00:00Z',
        routeActive: true,
      },
    ],
    requestClusterSnapshot: async () => SNAPSHOT,
    ...overrides,
  };
}

test('fetches every entity kind from its real endpoint shape and deep-links each', async () => {
  const namespaces: string[] = [];
  const client = mockClient({
    queryWorkflows: async (filter, _page, options) => {
      namespaces.push(options.namespace);
      return {
        items: filter.workflow_type === 'assistant' ? [ASSISTANT_SESSION] : [WORKFLOW],
        nextCursor: null,
        hasMore: false,
      };
    },
  });

  const result = await fetchPaletteEntities(client, {
    namespace: 'default' as Namespace,
    deployGranted: true,
  });

  expect(result.errors).toEqual([]);
  // Both workflow queries (all workflows + assistant sessions) are scoped to
  // the selected namespace.
  expect(namespaces).toEqual(['default', 'default']);

  const byKind = Object.groupBy(result.entities, (entity) => entity.kind);
  expect(byKind.workflow?.[0]?.href).toBe('/workflows/wf-123');
  expect(byKind.workflow?.[0]?.keywords).toEqual(['EmailDigest', 'Running']);
  // Assistant sessions deep-link to the assistant panel, not the raw workflow.
  expect(byKind.assistant?.[0]?.href).toBe('/assistant/wf-assist-1');
  expect(byKind.namespace?.map((entity) => entity.title)).toEqual(['default', 'orders']);
  // Packages deep-link to the start surface preloaded with the workflow type.
  expect(byKind.package?.[0]?.href).toBe('/actions?workflow_type=EmailDigest');
  expect(byKind.worker?.[0]?.title).toBe('worker-7');
  expect(byKind.worker?.[0]?.href).toBe('/failover');
});

test('deploy-scoped sources are not fetched without the runtime grant', async () => {
  let deployCalls = 0;
  const client = mockClient({
    listVersions: async () => {
      deployCalls += 1;
      return [];
    },
    requestClusterSnapshot: async () => {
      deployCalls += 1;
      return SNAPSHOT;
    },
  });

  const result = await fetchPaletteEntities(client, {
    namespace: 'default' as Namespace,
    deployGranted: false,
  });

  expect(deployCalls).toBe(0);
  expect(result.entities.some((entity) => entity.kind === 'package')).toBe(false);
  expect(result.entities.some((entity) => entity.kind === 'worker')).toBe(false);
});

test('a failing source surfaces as an error while the others still return', async () => {
  const client = mockClient({
    queryWorkflows: async () => {
      throw new Error('namespace_denied');
    },
  });

  const result = await fetchPaletteEntities(client, {
    namespace: 'default' as Namespace,
    deployGranted: true,
  });

  // Both sources on the denied endpoint fail visibly; the rest still return.
  expect(result.errors).toEqual([
    'workflows: namespace_denied',
    'assistant sessions: namespace_denied',
  ]);
  expect(result.entities.some((entity) => entity.kind === 'namespace')).toBe(true);
});

// --- grouping ---

async function sampleEntities() {
  const { entities } = await fetchPaletteEntities(mockClient(), {
    namespace: 'default' as Namespace,
    deployGranted: true,
  });
  return entities;
}

const RECENT: RecentSelection = {
  id: 'workflow:wf-old',
  kind: 'workflow',
  title: 'wf-old',
  subtitle: 'EmailDigest · Completed',
  href: '/workflows/wf-old',
};

test('everything mode groups by kind with headers in stable order', async () => {
  const groups = buildPaletteGroups({
    mode: 'everything',
    entities: await sampleEntities(),
    verbs: paletteVerbs(true),
    recents: [RECENT],
    query: '',
  });

  expect(groups.map((group) => group.heading)).toEqual([
    'Recent',
    'Actions',
    'Workflows',
    'Assistant sessions',
    'Namespaces',
    'Packages',
    'Workers',
  ]);
});

test('recents appear only on the empty prompt', async () => {
  const groups = buildPaletteGroups({
    mode: 'everything',
    entities: await sampleEntities(),
    verbs: paletteVerbs(true),
    recents: [RECENT],
    query: 'dig',
  });

  expect(groups.some((group) => group.heading === 'Recent')).toBe(false);
});

test('workflows mode narrows to workflow entities; actions mode to verbs', async () => {
  const entities = await sampleEntities();

  const workflows = buildPaletteGroups({
    mode: 'workflows',
    entities,
    verbs: paletteVerbs(true),
    recents: [RECENT],
    query: '',
  });
  expect(workflows.map((group) => group.heading)).toEqual(['Workflows']);

  const actions = buildPaletteGroups({
    mode: 'actions',
    entities,
    verbs: paletteVerbs(true),
    recents: [RECENT],
    query: '',
  });
  expect(actions.map((group) => group.heading)).toEqual(['Actions']);
  expect(actions[0]?.items.every((item) => item.kind === 'action')).toBe(true);
});

test('the deploy verb is offered only with the runtime deploy grant', () => {
  expect(paletteVerbs(true).some((verb) => verb.id === 'verb:deploy-package')).toBe(true);
  expect(paletteVerbs(false).some((verb) => verb.id === 'verb:deploy-package')).toBe(false);
});

test('the assistant verbs are always offered and land on the assistant panel', () => {
  const verbs = paletteVerbs(false);
  expect(verbs.find((verb) => verb.id === 'verb:new-assistant-session')?.href).toBe('/assistant');
  expect(verbs.find((verb) => verb.id === 'verb:go-assistant')?.href).toBe('/assistant');
});

// --- recents persistence ---

function memoryStorage(): StorageLike & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return {
    data,
    getItem: (key) => data.get(key) ?? null,
    setItem: (key, value) => {
      data.set(key, value);
    },
    removeItem: (key) => {
      data.delete(key);
    },
  };
}

test('recent selections persist, dedupe by id, and cap at eight', () => {
  const storage = memoryStorage();

  pushRecent(RECENT, storage);
  pushRecent({ ...RECENT, id: 'workflow:wf-2', title: 'wf-2' }, storage);
  // Re-selecting an existing entry moves it to the head without duplicating.
  const afterDedupe = pushRecent(RECENT, storage);
  expect(afterDedupe.map((entry) => entry.id)).toEqual(['workflow:wf-old', 'workflow:wf-2']);

  for (let index = 0; index < 10; index += 1) {
    pushRecent({ ...RECENT, id: `workflow:wf-${index}`, title: `wf-${index}` }, storage);
  }
  const capped = loadRecents(storage);
  expect(capped).toHaveLength(8);
  expect(capped[0]?.id).toBe('workflow:wf-9');

  // Round-trips through the persisted key.
  expect(storage.data.has(PALETTE_RECENTS_STORAGE_KEY)).toBe(true);
  const reloaded = loadRecents(storage);
  expect(reloaded).toEqual(capped);
});

test('corrupt persisted recents degrade to an empty list', () => {
  const storage = memoryStorage();
  storage.setItem(PALETTE_RECENTS_STORAGE_KEY, '{not json');

  expect(loadRecents(storage)).toEqual([]);
});
