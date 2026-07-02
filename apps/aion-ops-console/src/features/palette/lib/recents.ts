import type { StorageLike } from '@/lib/keybindings';

/** One remembered palette selection, enough to re-render and re-navigate. */
export type RecentSelection = {
  id: string;
  kind: string;
  title: string;
  subtitle: string;
  href: string;
};

export const PALETTE_RECENTS_STORAGE_KEY = 'aion:palette:recents';
const MAX_RECENTS = 8;

/** Load persisted recents; a corrupt blob degrades to an empty list. */
export function loadRecents(
  storage: StorageLike | undefined = defaultStorage()
): RecentSelection[] {
  const raw = storage?.getItem(PALETTE_RECENTS_STORAGE_KEY);
  if (raw === null || raw === undefined) {
    return [];
  }
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) {
      return [];
    }
    return parsed.filter(isRecentSelection).slice(0, MAX_RECENTS);
  } catch {
    return [];
  }
}

/**
 * Record a selection at the head of the recents list (deduped by id, capped at
 * {@link MAX_RECENTS}), persist, and return the new list.
 */
export function pushRecent(
  selection: RecentSelection,
  storage: StorageLike | undefined = defaultStorage()
): RecentSelection[] {
  const next = [
    selection,
    ...loadRecents(storage).filter((entry) => entry.id !== selection.id),
  ].slice(0, MAX_RECENTS);

  storage?.setItem(PALETTE_RECENTS_STORAGE_KEY, JSON.stringify(next));
  return next;
}

function isRecentSelection(value: unknown): value is RecentSelection {
  if (typeof value !== 'object' || value === null) {
    return false;
  }
  const record = value as Record<string, unknown>;
  return (
    typeof record.id === 'string' &&
    typeof record.kind === 'string' &&
    typeof record.title === 'string' &&
    typeof record.subtitle === 'string' &&
    typeof record.href === 'string'
  );
}

function defaultStorage(): StorageLike | undefined {
  return typeof localStorage === 'undefined' ? undefined : localStorage;
}
