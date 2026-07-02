import { useNavigate } from 'react-router';

import { ACTION_IDS, useAction } from '@/lib/keybindings';

import { namespacesPath, searchPath, workflowListPath } from './routePaths';

/**
 * Registers the shell's global action handlers with the central keybinding
 * registry: view jumps (g w / g e / g n) and focus-search (/). Mounted once by
 * the always-present AppShell so the actions are live app-wide; the palette
 * open action is registered by its own host. Renders nothing.
 */
export function GlobalActions() {
  const navigate = useNavigate();

  useAction(ACTION_IDS.goWorkflows, () => navigate(workflowListPath));
  useAction(ACTION_IDS.goEvents, () => navigate(searchPath));
  useAction(ACTION_IDS.goNamespaces, () => navigate(namespacesPath));
  useAction(ACTION_IDS.focusSearch, () => {
    navigate(searchPath);
    // Focus the search view's first field once it has mounted. The registry
    // already swallowed the "/" keystroke, so nothing leaks into the field.
    requestAnimationFrame(() => {
      document.querySelector<HTMLInputElement>('main input')?.focus();
    });
  });

  return null;
}
