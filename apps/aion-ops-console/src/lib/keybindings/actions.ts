import type { ActionDefinition } from './registry';

/**
 * The console's action inventory — every keyboard-reachable action, defined
 * once. Handlers are registered where the behavior lives (AppShell chrome for
 * navigation, the palette host for palette actions); components never define
 * bindings of their own. `mod` is ⌘ (macOS) / Super (Linux) — never Control.
 */
export const ACTION_IDS = {
  paletteOpen: 'palette.open',
  paletteClose: 'palette.close',
  paletteCycleMode: 'palette.cycle-mode',
  goWorkflows: 'go.workflows',
  goEvents: 'go.events',
  goNamespaces: 'go.namespaces',
  focusSearch: 'search.focus',
} as const;

export const CONSOLE_ACTIONS: readonly ActionDefinition[] = [
  {
    id: ACTION_IDS.paletteOpen,
    title: 'Open command palette',
    scope: 'global',
    defaultBinding: 'mod+k',
    // ⌘K is meta-modified, so firing from a focused field never eats a keystroke.
    allowInInputs: true,
  },
  {
    id: ACTION_IDS.paletteClose,
    title: 'Close command palette',
    scope: 'view',
    defaultBinding: 'escape',
    allowInInputs: true,
  },
  {
    id: ACTION_IDS.paletteCycleMode,
    title: 'Cycle palette mode',
    scope: 'view',
    defaultBinding: 'tab',
    allowInInputs: true,
  },
  {
    id: ACTION_IDS.goWorkflows,
    title: 'Go to workflows',
    scope: 'global',
    defaultBinding: 'g w',
  },
  {
    id: ACTION_IDS.goEvents,
    title: 'Go to events',
    scope: 'global',
    defaultBinding: 'g e',
  },
  {
    id: ACTION_IDS.goNamespaces,
    title: 'Go to namespaces',
    scope: 'global',
    defaultBinding: 'g n',
  },
  {
    id: ACTION_IDS.focusSearch,
    title: 'Focus search',
    scope: 'global',
    defaultBinding: '/',
  },
];
