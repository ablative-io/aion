import { generatePath } from 'react-router';

// Route path contract. Every parallel view-slice imports these constants and the
// *Href helpers (read-only); S0 is the sole owner/publisher. Kept dependency-free
// (no JSX, no AppShell) so the shell and the route table can both import it
// without a circular-init hazard.
export const workflowListPath = '/';
export const workflowDetailPath = '/workflows/:id';
export const searchPath = '/search';
export const incidentsPath = '/incidents';
export const failoverPath = '/failover';

export function workflowListHref() {
  return workflowListPath;
}

export function workflowDetailHref(workflowId: string, seq?: number) {
  const base = generatePath(workflowDetailPath, { id: workflowId });
  return seq === undefined ? base : `${base}?seq=${seq}`;
}

export function searchHref() {
  return searchPath;
}

export function incidentsHref() {
  return incidentsPath;
}

export function failoverHref() {
  return failoverPath;
}

export function routerBasenameFromBaseUrl(baseUrl = import.meta.env.BASE_URL ?? '/') {
  if (baseUrl === '' || baseUrl === '/') {
    return '/';
  }

  const withoutTrailingSlash = baseUrl.endsWith('/') ? baseUrl.slice(0, -1) : baseUrl;
  return withoutTrailingSlash.length === 0 ? '/' : withoutTrailingSlash;
}
