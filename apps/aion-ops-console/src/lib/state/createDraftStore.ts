import { create } from 'zustand';
import { type PersistStorage, persist, type StorageValue } from 'zustand/middleware';

import type { StorageLike } from '@/lib/keybindings';

/** Draft field bags are flat records of JSON-serializable values. */
export type DraftFields = Record<string, unknown>;

export type DraftState<T extends DraftFields> = {
  draft: T;
  /** Merge a partial edit into the draft (written through to sessionStorage). */
  setDraft: (patch: Partial<T>) => void;
  /** Reset to the pristine draft (e.g. after a confirmed submit). */
  clearDraft: () => void;
};

export type CreateDraftStoreOptions<T extends DraftFields> = {
  /** Persisted under `aion:draft:<name>` — keep it stable or existing drafts orphan. */
  name: string;
  /** The pristine draft; doubles as the shape template hydration is validated against. */
  empty: T;
  /** Storage seam for tests; defaults to sessionStorage when available. */
  storage?: StorageLike | undefined;
  /**
   * Validate/repair a hydrated draft blob (`unknown`, straight from storage).
   * Defaults to a shallow per-key typeof match against `empty`; override when a
   * field needs a deeper check (e.g. a record whose values must be strings).
   */
  sanitize?: (persisted: unknown, empty: T) => T;
};

export function draftStorageKey(name: string): string {
  return `aion:draft:${name}`;
}

/**
 * A zustand store for form-draft state, persisted to sessionStorage so a draft
 * survives navigation/unmount within the tab but never outlives it. Hydration
 * is defensive: a corrupt or wrong-shaped blob degrades to `empty` (never
 * throws), and an environment without sessionStorage (bun tests) degrades to
 * in-memory only. See ./README.md for what belongs in a draft store.
 */
export function createDraftStore<T extends DraftFields>({
  name,
  empty,
  storage,
  sanitize = sanitizeByShape,
}: CreateDraftStoreOptions<T>) {
  const seam = storage ?? defaultSessionStorage();

  return create<DraftState<T>>()(
    persist(
      (set) => ({
        draft: empty,
        setDraft: (patch) => set((state) => ({ draft: { ...state.draft, ...patch } })),
        clearDraft: () => set({ draft: empty }),
      }),
      {
        name: draftStorageKey(name),
        storage: guardedJsonStorage(seam),
        partialize: (state) => ({ draft: state.draft }),
        merge: (persisted, current) => ({
          ...current,
          draft: sanitize(isRecord(persisted) ? persisted.draft : undefined, empty),
        }),
      }
    )
  );
}

/** Keep only persisted keys that exist on `empty` with a matching primitive shape. */
function sanitizeByShape<T extends DraftFields>(persisted: unknown, empty: T): T {
  if (!isRecord(persisted)) {
    return empty;
  }

  const draft: DraftFields = { ...empty };
  for (const key of Object.keys(empty)) {
    const candidate = persisted[key];
    if (candidate !== null && typeof candidate === typeof empty[key]) {
      draft[key] = candidate;
    }
  }

  return draft as T;
}

/**
 * JSON storage adapter over the seam. Unlike zustand's `createJSONStorage`, a
 * missing seam is silent (no per-write console warnings in tests) and a blob
 * that is not the exact round-trip envelope — corrupt JSON, a foreign shape, a
 * version this code never wrote — hydrates as "nothing persisted" instead of
 * throwing or warning during store creation.
 */
function guardedJsonStorage<S>(seam: StorageLike | undefined): PersistStorage<S> {
  return {
    getItem: (key) => {
      const raw = seam?.getItem(key) ?? null;
      if (raw === null) {
        return null;
      }
      try {
        const parsed: unknown = JSON.parse(raw);
        if (!isRecord(parsed) || !('state' in parsed) || parsed.version !== 0) {
          return null;
        }
        return parsed as unknown as StorageValue<S>;
      } catch {
        return null;
      }
    },
    setItem: (key, value) => {
      seam?.setItem(key, JSON.stringify(value));
    },
    removeItem: (key) => {
      seam?.removeItem(key);
    },
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function defaultSessionStorage(): StorageLike | undefined {
  return typeof sessionStorage === 'undefined' ? undefined : sessionStorage;
}
