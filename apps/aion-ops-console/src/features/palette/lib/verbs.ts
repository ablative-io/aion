import {
  actionsPath,
  failoverPath,
  incidentsPath,
  namespacesPath,
  searchPath,
  workflowListPath,
} from '@/app/routePaths';

/**
 * A palette verb: an action result that NAVIGATES to the surface where the
 * operation lives (preloaded where the surface supports it). The palette never
 * performs a destructive or mutating operation directly — it takes you there.
 */
export type PaletteVerb = {
  id: string;
  title: string;
  keywords: string[];
  href: string;
};

const NAVIGATE_VERBS: readonly PaletteVerb[] = [
  {
    id: 'verb:go-workflows',
    title: 'Go to workflows',
    keywords: ['list', 'runs', 'navigate'],
    href: workflowListPath,
  },
  {
    id: 'verb:go-namespaces',
    title: 'Go to namespaces',
    keywords: ['registry', 'tenants', 'navigate'],
    href: namespacesPath,
  },
  {
    id: 'verb:go-search',
    title: 'Go to event search',
    keywords: ['events', 'find', 'navigate'],
    href: searchPath,
  },
  {
    id: 'verb:go-actions',
    title: 'Go to actions',
    keywords: ['operate', 'navigate'],
    href: actionsPath,
  },
  {
    id: 'verb:go-incidents',
    title: 'Go to incidents',
    keywords: ['triage', 'failures', 'navigate'],
    href: incidentsPath,
  },
  {
    id: 'verb:go-failover',
    title: 'Go to failover',
    keywords: ['cluster', 'nodes', 'navigate'],
    href: failoverPath,
  },
];

const START_WORKFLOW_VERB: PaletteVerb = {
  id: 'verb:start-workflow',
  title: 'Start a workflow…',
  keywords: ['run', 'launch', 'new'],
  href: actionsPath,
};

const DEPLOY_PACKAGE_VERB: PaletteVerb = {
  id: 'verb:deploy-package',
  title: 'Deploy a package…',
  keywords: ['upload', 'release', 'aion'],
  href: actionsPath,
};

/**
 * The verb inventory. Operational verbs land on the Actions surface; deploy is
 * offered only when the runtime-discovered capability grants it (the same gate
 * the Actions view renders under).
 */
export function paletteVerbs(deployGranted: boolean): PaletteVerb[] {
  return [START_WORKFLOW_VERB, ...(deployGranted ? [DEPLOY_PACKAGE_VERB] : []), ...NAVIGATE_VERBS];
}
