import { expect, test } from 'bun:test';

import { buildWebSocketUrl } from './websocket-protocol';

const ENDPOINT = '/events/stream';

test('buildWebSocketUrl appends no query when no credentials are given', () => {
  expect(buildWebSocketUrl('http://127.0.0.1:8090')).toBe(`ws://127.0.0.1:8090${ENDPOINT}`);
});

test('buildWebSocketUrl converts http(s) bases to ws(s)', () => {
  expect(buildWebSocketUrl('https://aion.example')).toBe(`wss://aion.example${ENDPOINT}`);
  expect(buildWebSocketUrl('ws://node:8090')).toBe(`ws://node:8090${ENDPOINT}`);
});

test('buildWebSocketUrl carries namespaces as a query param (browser cannot send headers)', () => {
  const url = buildWebSocketUrl('http://127.0.0.1:8090', { namespaces: ['default', 'ops'] });
  const parsed = new URL(url);

  expect(parsed.protocol).toBe('ws:');
  expect(parsed.pathname).toBe(ENDPOINT);
  // The comma-joined namespace list round-trips through URL decoding.
  expect(parsed.searchParams.get('x-aion-namespaces')).toBe('default,ops');
});

test('buildWebSocketUrl carries subject and bearer token (as access_token)', () => {
  const url = buildWebSocketUrl('http://127.0.0.1:8090', {
    namespaces: ['default'],
    subject: 'operator',
    bearerToken: 'jwt-abc.def',
  });
  const parsed = new URL(url);

  expect(parsed.searchParams.get('x-aion-namespaces')).toBe('default');
  expect(parsed.searchParams.get('x-aion-subject')).toBe('operator');
  expect(parsed.searchParams.get('access_token')).toBe('jwt-abc.def');
});

test('buildWebSocketUrl omits empty credential fields rather than sending blanks', () => {
  const url = buildWebSocketUrl('http://127.0.0.1:8090', { namespaces: [], subject: '' });

  // No namespaces, no subject => no query string at all (no fabricated blanks).
  expect(url).toBe(`ws://127.0.0.1:8090${ENDPOINT}`);
});
