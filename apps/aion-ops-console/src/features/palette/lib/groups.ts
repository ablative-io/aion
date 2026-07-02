import type { PaletteEntity, PaletteEntityKind } from './entities';
import type { PaletteMode } from './modes';
import type { RecentSelection } from './recents';
import type { PaletteVerb } from './verbs';

/** One renderable palette row, normalized across entities/verbs/recents. */
export type PaletteItem = {
  id: string;
  kind: PaletteEntityKind | 'action' | 'recent';
  title: string;
  subtitle: string;
  keywords: string[];
  href: string;
};

export type PaletteGroup = {
  heading: string;
  items: PaletteItem[];
};

export type PaletteGroupInput = {
  mode: PaletteMode;
  entities: PaletteEntity[];
  verbs: PaletteVerb[];
  recents: RecentSelection[];
  /** The live query text — recents show only on the empty prompt. */
  query: string;
};

const ENTITY_HEADINGS: Record<PaletteEntityKind, string> = {
  workflow: 'Workflows',
  namespace: 'Namespaces',
  package: 'Packages',
  worker: 'Workers',
};

const ENTITY_ORDER: readonly PaletteEntityKind[] = ['workflow', 'namespace', 'package', 'worker'];

/**
 * Assemble the palette's grouped result set for a mode: `everything` = recents
 * (empty prompt only) + actions + every entity kind, `workflows` = workflow
 * entities only, `actions` = verbs only. Groups keep a stable order with
 * per-kind headings; empty groups are dropped. Query-level fuzzy filtering is
 * cmdk's job — this shapes WHAT is searchable, not what matched.
 */
export function buildPaletteGroups(input: PaletteGroupInput): PaletteGroup[] {
  const groups: PaletteGroup[] = [];

  if (input.mode === 'everything' && input.query.trim().length === 0) {
    groups.push({ heading: 'Recent', items: input.recents.map(recentToItem) });
  }

  if (input.mode !== 'workflows') {
    groups.push({ heading: 'Actions', items: input.verbs.map(verbToItem) });
  }

  if (input.mode !== 'actions') {
    const kinds = input.mode === 'workflows' ? (['workflow'] as const) : ENTITY_ORDER;
    for (const kind of kinds) {
      groups.push({
        heading: ENTITY_HEADINGS[kind],
        items: input.entities.filter((entity) => entity.kind === kind).map(entityToItem),
      });
    }
  }

  return groups.filter((group) => group.items.length > 0);
}

function entityToItem(entity: PaletteEntity): PaletteItem {
  return { ...entity };
}

function verbToItem(verb: PaletteVerb): PaletteItem {
  return { ...verb, kind: 'action', subtitle: 'action' };
}

function recentToItem(recent: RecentSelection): PaletteItem {
  return { ...recent, kind: 'recent', keywords: [recent.kind] };
}
