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

test('buildWebSocketUrl carries the deploy grant as x-aion-deploy for the cluster stream', () => {
  // The deploy-scoped cluster (WS3) socket must carry the deployment-wide grant
  // or the server denies it (cluster_stream.rs); a browser cannot set the header
  // on a WS handshake, so it rides as the query param the server promotes.
  const url = buildWebSocketUrl('http://127.0.0.1:8090', { namespaces: ['default'], deploy: true });

  expect(new URL(url).searchParams.get('x-aion-deploy')).toBe('true');
});

test('buildWebSocketUrl never sends x-aion-deploy unless explicitly granted', () => {
  // The namespace-scoped workflow event socket must NOT carry a deploy grant.
  const withoutFlag = buildWebSocketUrl('http://127.0.0.1:8090', { namespaces: ['default'] });
  expect(new URL(withoutFlag).searchParams.has('x-aion-deploy')).toBe(false);

  const explicitlyFalse = buildWebSocketUrl('http://127.0.0.1:8090', {
    namespaces: ['default'],
    deploy: false,
  });
  expect(new URL(explicitlyFalse).searchParams.has('x-aion-deploy')).toBe(false);
});
