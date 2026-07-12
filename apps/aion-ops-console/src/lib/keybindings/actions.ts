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
  goAssistant: 'go.assistant',
  focusSearch: 'search.focus',
  assistantFocusChat: 'assistant.focus-chat',
  authoringSave: 'authoring.save',
  authoringViewText: 'authoring.view-text',
  authoringViewCanvas: 'authoring.view-canvas',
  authoringViewSplit: 'authoring.view-split',
  authoringAddStep: 'authoring.add-step',
  authoringAddAction: 'authoring.add-action',
  authoringAddRoute: 'authoring.add-route',
  authoringAddFallThrough: 'authoring.add-fall-through',
  authoringEditProse: 'authoring.edit-prose',
  authoringRenameBinding: 'authoring.rename-binding',
  authoringDeleteStep: 'authoring.delete-step',
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
    id: ACTION_IDS.goAssistant,
    title: 'Go to assistant',
    scope: 'global',
    defaultBinding: 'g a',
  },
  {
    id: ACTION_IDS.focusSearch,
    title: 'Focus search',
    scope: 'global',
    defaultBinding: '/',
  },
  {
    id: ACTION_IDS.assistantFocusChat,
    title: 'Focus assistant chat',
    scope: 'view',
    defaultBinding: 'mod+i',
    // ⌘I is meta-modified, so firing from a focused field never eats a keystroke.
    allowInInputs: true,
  },
  {
    id: ACTION_IDS.authoringSave,
    title: 'Save authoring document',
    scope: 'view',
    defaultBinding: 'mod+s',
    // Saving must work while the CodeMirror contenteditable owns focus.
    allowInInputs: true,
  },
  {
    id: ACTION_IDS.authoringViewText,
    title: 'Show authoring text view',
    scope: 'view',
    defaultBinding: 'mod+1',
    allowInInputs: true,
  },
  {
    id: ACTION_IDS.authoringViewCanvas,
    title: 'Show authoring canvas view',
    scope: 'view',
    defaultBinding: 'mod+2',
    allowInInputs: true,
  },
  {
    id: ACTION_IDS.authoringViewSplit,
    title: 'Show split authoring view',
    scope: 'view',
    defaultBinding: 'mod+3',
    allowInInputs: true,
  },
  {
    id: ACTION_IDS.authoringAddStep,
    title: 'Canvas: add step',
    scope: 'view',
    defaultBinding: 'mod+shift+n',
  },
  {
    id: ACTION_IDS.authoringAddAction,
    title: 'Canvas: add action contract',
    scope: 'view',
    defaultBinding: 'mod+shift+a',
  },
  {
    id: ACTION_IDS.authoringAddRoute,
    title: 'Canvas: connect outcome route',
    scope: 'view',
    defaultBinding: 'mod+shift+r',
  },
  {
    id: ACTION_IDS.authoringAddFallThrough,
    title: 'Canvas: connect fall-through',
    scope: 'view',
    defaultBinding: 'mod+shift+f',
  },
  {
    id: ACTION_IDS.authoringEditProse,
    title: 'Canvas: edit step prose',
    scope: 'view',
    defaultBinding: 'mod+shift+e',
  },
  {
    id: ACTION_IDS.authoringRenameBinding,
    title: 'Canvas: rename binding',
    scope: 'view',
    defaultBinding: 'mod+shift+m',
  },
  {
    id: ACTION_IDS.authoringDeleteStep,
    title: 'Canvas: delete step',
    scope: 'view',
    defaultBinding: 'mod+shift+backspace',
  },
];
