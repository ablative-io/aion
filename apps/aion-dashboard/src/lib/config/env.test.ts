import { expect, test } from 'bun:test';

import { buildCredentials, type DashboardEnv, parseDashboardConfig } from './env';

test('parseDashboardConfig defaults to same origin (empty bases) when env is empty', () => {
  const config = parseDashboardConfig({});

  // Unset => same origin: the embedded console must call the node that served it.
  expect(config.apiBaseUrl).toBe('');
  expect(config.wsBaseUrl).toBe('');
  expect(config.namespaces).toEqual([]);
  expect(config.subject).toBeUndefined();
  expect(config.bearerToken).toBeUndefined();
});

test('parseDashboardConfig reads explicit bases, namespaces, subject and token', () => {
  const env: DashboardEnv = {
    VITE_AION_API_BASE: 'https://aion.example/',
    VITE_AION_WS_BASE: 'wss://aion.example/stream/',
    VITE_AION_NAMESPACES: 'default, ops ,, billing',
    VITE_AION_SUBJECT: 'operator',
    VITE_AION_BEARER_TOKEN: 'jwt-token',
  };

  const config = parseDashboardConfig(env);

  // Trailing slashes stripped.
  expect(config.apiBaseUrl).toBe('https://aion.example');
  expect(config.wsBaseUrl).toBe('wss://aion.example/stream');
  // Comma list split, trimmed, blanks dropped.
  expect(config.namespaces).toEqual(['default', 'ops', 'billing']);
  expect(config.subject).toBe('operator');
  expect(config.bearerToken).toBe('jwt-token');
});

test('parseDashboardConfig derives the WS base from the API base when WS is unset', () => {
  expect(parseDashboardConfig({ VITE_AION_API_BASE: 'http://node:8090' }).wsBaseUrl).toBe(
    'ws://node:8090'
  );
  expect(parseDashboardConfig({ VITE_AION_API_BASE: 'https://node:8443' }).wsBaseUrl).toBe(
    'wss://node:8443'
  );
});

test('parseDashboardConfig treats blank values as unset (no silent empty credentials)', () => {
  const config = parseDashboardConfig({
    VITE_AION_API_BASE: '   ',
    VITE_AION_SUBJECT: '  ',
    VITE_AION_BEARER_TOKEN: '',
    VITE_AION_NAMESPACES: '   ,  ',
  });

  // Blank == unset == same origin (empty base).
  expect(config.apiBaseUrl).toBe('');
  expect(config.subject).toBeUndefined();
  expect(config.bearerToken).toBeUndefined();
  expect(config.namespaces).toEqual([]);
});

test('parseDashboardConfig reads the deploy grant strictly (only literal true grants)', () => {
  // Off by default — the deploy-scoped cluster stream must not be implicitly granted.
  expect(parseDashboardConfig({}).deployGranted).toBe(false);
  // Strict true (case-insensitive, trimmed), mirroring the server's x-aion-deploy parsing.
  expect(parseDashboardConfig({ VITE_AION_DEPLOY: 'true' }).deployGranted).toBe(true);
  expect(parseDashboardConfig({ VITE_AION_DEPLOY: ' TRUE ' }).deployGranted).toBe(true);
  // Anything else is no grant.
  expect(parseDashboardConfig({ VITE_AION_DEPLOY: 'false' }).deployGranted).toBe(false);
  expect(parseDashboardConfig({ VITE_AION_DEPLOY: '1' }).deployGranted).toBe(false);
  expect(parseDashboardConfig({ VITE_AION_DEPLOY: '' }).deployGranted).toBe(false);
});

test('buildCredentials returns undefined when nothing is configured', () => {
  const config = parseDashboardConfig({});

  expect(buildCredentials(config)).toBeUndefined();
});

test('buildCredentials carries the full grant when no namespace is selected', () => {
  const config = parseDashboardConfig({
    VITE_AION_NAMESPACES: 'default,ops',
    VITE_AION_SUBJECT: 'operator',
  });

  expect(buildCredentials(config)).toEqual({
    namespaces: ['default', 'ops'],
    subject: 'operator',
  });
});

test('buildCredentials scopes namespaces to the selected namespace when provided', () => {
  const config = parseDashboardConfig({ VITE_AION_NAMESPACES: 'default,ops' });

  expect(buildCredentials(config, 'ops')).toEqual({ namespaces: ['ops'] });
});

test('buildCredentials includes a selected namespace not present in the grant', () => {
  const config = parseDashboardConfig({ VITE_AION_NAMESPACES: 'default' });

  // A selected namespace the grant does not list is still sent (plus the grant)
  // so the request is never silently scoped away from what the user is viewing.
  expect(buildCredentials(config, 'tenant-x')).toEqual({ namespaces: ['tenant-x', 'default'] });
});

test('buildCredentials carries bearer token without namespaces when only token is set', () => {
  const config = parseDashboardConfig({ VITE_AION_BEARER_TOKEN: 'jwt' });

  expect(buildCredentials(config)).toEqual({ bearerToken: 'jwt' });
});

test('buildCredentials carries the deploy grant when VITE_AION_DEPLOY=true', () => {
  const config = parseDashboardConfig({
    VITE_AION_NAMESPACES: 'default',
    VITE_AION_SUBJECT: 'operator',
    VITE_AION_DEPLOY: 'true',
  });

  expect(buildCredentials(config)).toEqual({
    namespaces: ['default'],
    subject: 'operator',
    deployGranted: true,
  });
});

test('buildCredentials omits the deploy grant when it is not granted', () => {
  const config = parseDashboardConfig({ VITE_AION_NAMESPACES: 'default' });

  expect(buildCredentials(config)).toEqual({ namespaces: ['default'] });
});
