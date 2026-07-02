import { useNavigate } from 'react-router';

import { FOCUS_SEARCH_STATE } from '@/features/search';
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
  // Chrome only navigates; the search view owns focusing its own field (it
  // reads this router state after its lazy chunk mounts, and while mounted it
  // registers its own later handler that wins registry precedence).
  useAction(ACTION_IDS.focusSearch, () => navigate(searchPath, { state: FOCUS_SEARCH_STATE }));

  return null;
}
