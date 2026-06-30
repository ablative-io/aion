import { expect, test } from 'bun:test';

import { buildCredentials, type OpsConsoleEnv, parseOpsConsoleConfig } from './env';

test('parseOpsConsoleConfig defaults to same origin (empty bases) when env is empty', () => {
  const config = parseOpsConsoleConfig({});

  // Unset => same origin: the embedded console must call the node that served it.
  expect(config.apiBaseUrl).toBe('');
  expect(config.wsBaseUrl).toBe('');
  expect(config.namespaces).toEqual([]);
  expect(config.subject).toBeUndefined();
  expect(config.bearerToken).toBeUndefined();
});

test('parseOpsConsoleConfig reads explicit bases, namespaces, subject and token', () => {
  const env: OpsConsoleEnv = {
    VITE_AION_API_BASE: 'https://aion.example/',
    VITE_AION_WS_BASE: 'wss://aion.example/stream/',
    VITE_AION_NAMESPACES: 'default, ops ,, billing',
    VITE_AION_SUBJECT: 'operator',
    VITE_AION_BEARER_TOKEN: 'jwt-token',
  };

  const config = parseOpsConsoleConfig(env);

  // Trailing slashes stripped.
  expect(config.apiBaseUrl).toBe('https://aion.example');
  expect(config.wsBaseUrl).toBe('wss://aion.example/stream');
  // Comma list split, trimmed, blanks dropped.
  expect(config.namespaces).toEqual(['default', 'ops', 'billing']);
  expect(config.subject).toBe('operator');
  expect(config.bearerToken).toBe('jwt-token');
});

test('parseOpsConsoleConfig derives the WS base from the API base when WS is unset', () => {
  expect(parseOpsConsoleConfig({ VITE_AION_API_BASE: 'http://node:8090' }).wsBaseUrl).toBe(
    'ws://node:8090'
  );
  expect(parseOpsConsoleConfig({ VITE_AION_API_BASE: 'https://node:8443' }).wsBaseUrl).toBe(
    'wss://node:8443'
  );
});

test('parseOpsConsoleConfig treats blank values as unset (no silent empty credentials)', () => {
  const config = parseOpsConsoleConfig({
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

test('parseOpsConsoleConfig bakes no deploy grant (authorization is discovered at runtime)', () => {
  // The config carries no deploy field at all: deploy is authorized server-side
  // (operator mode) or by the bearer token's deploy claim (real auth), and the
  // console discovers the resulting grant at runtime via GET /whoami. A stale
  // VITE_AION_DEPLOY env value must NOT influence authorization.
  const config = parseOpsConsoleConfig({ VITE_AION_DEPLOY: 'true' } as never);
  expect('deployGranted' in config).toBe(false);
});

test('buildCredentials returns undefined when nothing is configured', () => {
  const config = parseOpsConsoleConfig({});

  expect(buildCredentials(config)).toBeUndefined();
});

test('buildCredentials carries the full grant when no namespace is selected', () => {
  const config = parseOpsConsoleConfig({
    VITE_AION_NAMESPACES: 'default,ops',
    VITE_AION_SUBJECT: 'operator',
  });

  expect(buildCredentials(config)).toEqual({
    namespaces: ['default', 'ops'],
    subject: 'operator',
  });
});

test('buildCredentials scopes namespaces to the selected namespace when provided', () => {
  const config = parseOpsConsoleConfig({ VITE_AION_NAMESPACES: 'default,ops' });

  expect(buildCredentials(config, 'ops')).toEqual({ namespaces: ['ops'] });
});

test('buildCredentials includes a selected namespace not present in the grant', () => {
  const config = parseOpsConsoleConfig({ VITE_AION_NAMESPACES: 'default' });

  // A selected namespace the grant does not list is still sent (plus the grant)
  // so the request is never silently scoped away from what the user is viewing.
  expect(buildCredentials(config, 'tenant-x')).toEqual({ namespaces: ['tenant-x', 'default'] });
});

test('buildCredentials carries bearer token without namespaces when only token is set', () => {
  const config = parseOpsConsoleConfig({ VITE_AION_BEARER_TOKEN: 'jwt' });

  expect(buildCredentials(config)).toEqual({ bearerToken: 'jwt' });
});

test('buildCredentials never bakes a deploy grant into the credentials', () => {
  // Even with a stale VITE_AION_DEPLOY in the environment, the credentials carry
  // no deploy flag: the console asserts nothing about the deploy grant.
  const config = parseOpsConsoleConfig({
    VITE_AION_NAMESPACES: 'default',
    VITE_AION_SUBJECT: 'operator',
    VITE_AION_DEPLOY: 'true',
  } as never);

  const credentials = buildCredentials(config);
  expect(credentials).toEqual({ namespaces: ['default'], subject: 'operator' });
  expect(credentials && 'deployGranted' in credentials).toBe(false);
});
