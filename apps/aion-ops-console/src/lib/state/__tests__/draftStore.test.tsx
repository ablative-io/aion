import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { StorageLike } from '@/lib/keybindings';
import { createDraftStore, type DraftStoreHandle, draftStorageKey, useDraft } from '@/lib/state';

type FakeStorage = StorageLike & { map: Map<string, string> };

function makeStorage(seed: Record<string, string> = {}): FakeStorage {
  const map = new Map(Object.entries(seed));

  return {
    map,
    getItem: (key) => map.get(key) ?? null,
    setItem: (key, value) => {
      map.set(key, value);
    },
    removeItem: (key) => {
      map.delete(key);
    },
  };
}

const EMPTY = { query: '', mode: 'plain' };

describe('createDraftStore', () => {
  test('a draft written through one store hydrates into a fresh store (reload)', () => {
    const storage = makeStorage();
    const first = createDraftStore({ name: 'persists', empty: EMPTY, storage });
    first.getState().setDraft({ query: 'shard map' });

    // A new store over the same storage is a page reload: the draft survives.
    const second = createDraftStore({ name: 'persists', empty: EMPTY, storage });
    expect(second.getState().draft).toEqual({ query: 'shard map', mode: 'plain' });
  });

  test('clearDraft resets the state and the persisted blob', () => {
    const storage = makeStorage();
    const store = createDraftStore({ name: 'clears', empty: EMPTY, storage });
    store.getState().setDraft({ query: 'to be cleared', mode: 'regex' });
    store.getState().clearDraft();

    expect(store.getState().draft).toEqual(EMPTY);
    const rehydrated = createDraftStore({ name: 'clears', empty: EMPTY, storage });
    expect(rehydrated.getState().draft).toEqual(EMPTY);
  });

  test('a corrupt sessionStorage blob degrades to the empty draft, never a crash', () => {
    const storage = makeStorage({ [draftStorageKey('corrupt')]: 'not json {{{' });
    const store = createDraftStore({ name: 'corrupt', empty: EMPTY, storage });

    expect(store.getState().draft).toEqual(EMPTY);
    // The store still works for new drafts after the bad blob.
    store.getState().setDraft({ query: 'recovered' });
    expect(store.getState().draft.query).toBe('recovered');
  });

  test('a wrong-shaped blob is sanitized per field (bad types dropped, good kept)', () => {
    const blob = JSON.stringify({
      state: { draft: { query: 42, mode: 'regex', stowaway: 'dropped' } },
      version: 0,
    });
    const storage = makeStorage({ [draftStorageKey('shape')]: blob });
    const store = createDraftStore({ name: 'shape', empty: EMPTY, storage });

    expect(store.getState().draft).toEqual({ query: '', mode: 'regex' });
  });

  test('a foreign envelope (non-object state, unknown version) hydrates as empty', () => {
    const storage = makeStorage({
      [draftStorageKey('foreign')]: JSON.stringify({ state: { draft: EMPTY }, version: 7 }),
    });
    const store = createDraftStore({ name: 'foreign', empty: EMPTY, storage });

    expect(store.getState().draft).toEqual(EMPTY);
  });

  test('without sessionStorage (this test env) the store degrades to in-memory', () => {
    const store = createDraftStore({ name: 'memory-only', empty: EMPTY });
    store.getState().setDraft({ query: 'in memory' });

    expect(store.getState().draft.query).toBe('in memory');
  });
});

function Probe({ store }: { store: DraftStoreHandle<typeof EMPTY> }) {
  const { draft } = useDraft(store);

  return <output data-slot="probe">{draft.query}</output>;
}

describe('useDraft', () => {
  test('a draft survives unmount/remount: the second mount seeds from the store', () => {
    const store = createDraftStore({ name: 'remount', empty: EMPTY, storage: makeStorage() });

    // First mount starts pristine.
    expect(renderToStaticMarkup(<Probe store={store} />)).not.toContain('drafted-query');

    // The user types (write-through), the component unmounts, a remount restores.
    store.getState().setDraft({ query: 'drafted-query' });
    expect(renderToStaticMarkup(<Probe store={store} />)).toContain('drafted-query');
  });

  test('after clearDraft a remount is pristine again', () => {
    const store = createDraftStore({ name: 'remount-clear', empty: EMPTY, storage: makeStorage() });
    store.getState().setDraft({ query: 'stale' });
    store.getState().clearDraft();

    expect(renderToStaticMarkup(<Probe store={store} />)).not.toContain('stale');
  });
});
