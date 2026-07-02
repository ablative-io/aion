import { expect, test } from 'bun:test';

import { formatBinding, type KeyEventLike, parseBinding } from '../bindings';
import {
  type ActionDefinition,
  KEYBINDINGS_STORAGE_KEY,
  KeybindingRegistry,
  type StorageLike,
} from '../registry';

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

function keyEvent(overrides: Partial<KeyEventLike> & { key: string }): KeyEventLike {
  return {
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    target: null,
    preventDefault: () => undefined,
    ...overrides,
  };
}

const ACTIONS: ActionDefinition[] = [
  {
    id: 'palette.open',
    title: 'Open palette',
    scope: 'global',
    defaultBinding: 'mod+k',
    allowInInputs: true,
  },
  { id: 'go.workflows', title: 'Go to workflows', scope: 'global', defaultBinding: 'g w' },
  { id: 'list.down', title: 'Down', scope: 'view', defaultBinding: 'j' },
  { id: 'chrome.down', title: 'Chrome down', scope: 'global', defaultBinding: 'j' },
];

function makeRegistry(storage: StorageLike = memoryStorage(), now?: () => number) {
  return new KeybindingRegistry({
    actions: ACTIONS,
    platform: 'mac',
    storage,
    ...(now === undefined ? {} : { now }),
  });
}

// --- scope resolution ---

test('a view-scoped handler wins over a global handler on the same binding', () => {
  const registry = makeRegistry();
  const fired: string[] = [];
  registry.registerHandler('chrome.down', () => fired.push('global'));
  const unregisterView = registry.registerHandler('list.down', () => fired.push('view'));

  expect(registry.dispatch(keyEvent({ key: 'j' }))).toBe(true);
  expect(fired).toEqual(['view']);

  // Once the view unmounts, the same key falls through to the global action.
  unregisterView();
  expect(registry.dispatch(keyEvent({ key: 'j' }))).toBe(true);
  expect(fired).toEqual(['view', 'global']);
});

test('an action with no registered handler is inert', () => {
  const registry = makeRegistry();

  expect(registry.dispatch(keyEvent({ key: 'j' }))).toBe(false);
});

// --- override persistence ---

test('overrides persist to storage and rebind matching', () => {
  const storage = memoryStorage();
  const registry = makeRegistry(storage);
  const fired: string[] = [];
  registry.registerHandler('palette.open', () => fired.push('palette'));

  registry.setOverride('palette.open', 'mod+p');

  // The old binding no longer fires; the new one does.
  expect(registry.dispatch(keyEvent({ key: 'k', metaKey: true }))).toBe(false);
  expect(registry.dispatch(keyEvent({ key: 'p', metaKey: true }))).toBe(true);
  expect(fired).toEqual(['palette']);

  // Persisted under the contract key, and a NEW registry (a reload) picks it up.
  expect(JSON.parse(storage.data.get(KEYBINDINGS_STORAGE_KEY) ?? '{}')).toEqual({
    'palette.open': 'mod+p',
  });
  const reloaded = makeRegistry(storage);
  reloaded.registerHandler('palette.open', () => fired.push('reloaded'));
  expect(reloaded.dispatch(keyEvent({ key: 'p', metaKey: true }))).toBe(true);
  expect(reloaded.bindingFor('palette.open')).toBe('mod+p');

  // clearOverride restores the default and removes the persisted entry.
  reloaded.clearOverride('palette.open');
  expect(reloaded.dispatch(keyEvent({ key: 'k', metaKey: true }))).toBe(true);
  expect(JSON.parse(storage.data.get(KEYBINDINGS_STORAGE_KEY) ?? '{}')).toEqual({});
});

test('an unparseable persisted override falls back to the default on BOTH paths', () => {
  const storage = memoryStorage();
  // Hand-edited localStorage is the sanctioned remap path today; a typo'd or
  // forbidden spec must degrade to the default, never crash chrome rendering.
  storage.setItem(
    KEYBINDINGS_STORAGE_KEY,
    JSON.stringify({ 'palette.open': 'ctrl+k', 'go.workflows': 'total garbage spec here' })
  );
  const registry = makeRegistry(storage);
  const fired: string[] = [];
  registry.registerHandler('palette.open', () => fired.push('palette'));

  // Display path: no throw, default binding shown.
  expect(registry.bindingFor('palette.open')).toBe('mod+k');
  expect(registry.formatFor('palette.open')).toBe('⌘K');
  expect(registry.formatFor('go.workflows')).toBe('G W');

  // Dispatch path agrees with what the display claims.
  expect(registry.dispatch(keyEvent({ key: 'k', metaKey: true }))).toBe(true);
  expect(fired).toEqual(['palette']);
});

test('a valid persisted override still wins on the display path', () => {
  const storage = memoryStorage();
  storage.setItem(KEYBINDINGS_STORAGE_KEY, JSON.stringify({ 'palette.open': 'mod+p' }));
  const registry = makeRegistry(storage);

  expect(registry.bindingFor('palette.open')).toBe('mod+p');
  expect(registry.formatFor('palette.open')).toBe('⌘P');
});

// --- platform modifier formatting ---

test('formats mod as ⌘ on macOS and Sup+ on Linux — never Control', () => {
  expect(formatBinding('mod+k', 'mac')).toBe('⌘K');
  expect(formatBinding('mod+k', 'linux')).toBe('Sup+K');
  expect(formatBinding('g w', 'mac')).toBe('G W');
});

test('rejects Control as a modifier at definition time', () => {
  expect(() => parseBinding('ctrl+k')).toThrow('Control');
});

test('a control-modified event never matches a mod binding', () => {
  const registry = makeRegistry();
  let fired = 0;
  registry.registerHandler('palette.open', () => {
    fired += 1;
  });

  expect(registry.dispatch(keyEvent({ key: 'k', ctrlKey: true }))).toBe(false);
  expect(registry.dispatch(keyEvent({ key: 'k', metaKey: true, ctrlKey: true }))).toBe(false);
  expect(fired).toBe(0);
});

// --- input-focus suppression ---

test('plain-key bindings never fire from an editable target; opt-in bindings do', () => {
  const registry = makeRegistry();
  const fired: string[] = [];
  registry.registerHandler('list.down', () => fired.push('down'));
  registry.registerHandler('palette.open', () => fired.push('palette'));

  const input = { tagName: 'INPUT' };
  expect(registry.dispatch(keyEvent({ key: 'j', target: input }))).toBe(false);
  const editable = { isContentEditable: true };
  expect(registry.dispatch(keyEvent({ key: 'j', target: editable }))).toBe(false);

  // allowInInputs opts in: ⌘K still opens the palette from a field.
  expect(registry.dispatch(keyEvent({ key: 'k', metaKey: true, target: input }))).toBe(true);
  expect(fired).toEqual(['palette']);
});

// --- chords ---

test('two-step chords dispatch on the second key and expire after the timeout', () => {
  let clock = 0;
  const registry = makeRegistry(memoryStorage(), () => clock);
  const fired: string[] = [];
  registry.registerHandler('go.workflows', () => fired.push('workflows'));

  // g then w within the window fires.
  expect(registry.dispatch(keyEvent({ key: 'g' }))).toBe(true);
  clock += 500;
  expect(registry.dispatch(keyEvent({ key: 'w' }))).toBe(true);
  expect(fired).toEqual(['workflows']);

  // An expired prefix does not fire.
  expect(registry.dispatch(keyEvent({ key: 'g' }))).toBe(true);
  clock += 5_000;
  expect(registry.dispatch(keyEvent({ key: 'w' }))).toBe(false);
  expect(fired).toEqual(['workflows']);

  // A wrong second key clears the pending prefix.
  expect(registry.dispatch(keyEvent({ key: 'g' }))).toBe(true);
  expect(registry.dispatch(keyEvent({ key: 'x' }))).toBe(false);
  expect(registry.dispatch(keyEvent({ key: 'w' }))).toBe(false);
  expect(fired).toEqual(['workflows']);
});

test('chord prefixes are suppressed in editable targets too', () => {
  const registry = makeRegistry();
  const fired: string[] = [];
  registry.registerHandler('go.workflows', () => fired.push('workflows'));

  // Typing "g w" in a text field must not navigate.
  const input = { tagName: 'TEXTAREA' };
  expect(registry.dispatch(keyEvent({ key: 'g', target: input }))).toBe(false);
  expect(registry.dispatch(keyEvent({ key: 'w', target: input }))).toBe(false);
  expect(fired).toEqual([]);
});
