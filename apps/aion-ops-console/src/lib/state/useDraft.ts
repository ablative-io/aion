import { useCallback, useState } from 'react';
import type { StoreApi } from 'zustand';

import type { DraftFields, DraftState } from './createDraftStore';

/** The slice of a draft store the hook needs — keeps callers mockable in tests. */
export type DraftStoreHandle<T extends DraftFields> = Pick<StoreApi<DraftState<T>>, 'getState'>;

export type UseDraftResult<T extends DraftFields> = {
  draft: T;
  updateDraft: (patch: Partial<T>) => void;
  clearDraft: () => void;
};

/**
 * Controlled-input bridge onto a draft store: local state is seeded from the
 * persisted draft at mount (returning to the form restores what was typed) and
 * written through on every change (an unmount can never eat it). Local state —
 * not a store subscription — keeps render behavior identical to the plain
 * useState forms this replaces, and keeps server rendering (the repo's test
 * idiom) reading the hydrated draft; useSyncExternalStore would render the
 * pre-hydration initial snapshot there.
 */
export function useDraft<T extends DraftFields>(store: DraftStoreHandle<T>): UseDraftResult<T> {
  const [draft, setDraft] = useState<T>(() => store.getState().draft);

  const updateDraft = useCallback(
    (patch: Partial<T>) => {
      setDraft((previous) => ({ ...previous, ...patch }));
      store.getState().setDraft(patch);
    },
    [store]
  );

  const clearDraft = useCallback(() => {
    store.getState().clearDraft();
    setDraft(store.getState().draft);
  }, [store]);

  return { draft, updateDraft, clearDraft };
}
