import { describe, expect, mock, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { ConnectionStatus } from '@/lib/api';

import { ConnectionIndicatorContent } from './ConnectionIndicator';

let currentStatus: ConnectionStatus = 'disconnected';

mock.module('../hooks/useConnectionStatus', () => ({
  useConnectionStatus: () => currentStatus,
  useSocketError: () => null,
}));

describe('ConnectionIndicator', () => {
  test('renders healthy, possible-gap, reconnecting, and disconnected states', async () => {
    const { ConnectionIndicator } = await import('./ConnectionIndicator');
    const statuses = [
      'connected',
      'resynced-with-possible-gap',
      'reconnecting',
      'disconnected',
    ] satisfies ConnectionStatus[];
    const markups = statuses.map((status) => {
      currentStatus = status;
      return renderToStaticMarkup(<ConnectionIndicator />);
    });

    expect(new Set(markups).size).toBe(statuses.length);
    for (const status of statuses) {
      currentStatus = status;
      const markup = renderToStaticMarkup(<ConnectionIndicator />);

      expect(markup).toContain(`data-connection-status="${status}"`);
      expect(markup).toContain(connectionLabel(status));
    }
  });

  test('ConnectionIndicatorContent surfaces a socket error message as visible state', () => {
    const markup = renderToStaticMarkup(
      <ConnectionIndicatorContent
        error={{
          kind: 'frame-decode',
          subscriptionId: 'aion-events-1',
          message: 'A live event could not be decoded; the feed may be missing entries.',
          cause: null,
        }}
        status="connected"
      />
    );

    expect(markup).toContain('A live event could not be decoded');
    expect(markup).toContain('data-socket-error="frame-decode"');
  });

  test('ConnectionIndicatorContent omits the error region when healthy', () => {
    const markup = renderToStaticMarkup(<ConnectionIndicatorContent status="connected" />);

    expect(markup).not.toContain('data-socket-error');
  });
});

function connectionLabel(status: ConnectionStatus): string {
  switch (status) {
    case 'connected':
      return 'Connected';
    case 'resynced-with-possible-gap':
      return 'Live with possible gap';
    case 'reconnecting':
      return 'Reconnecting';
    case 'disconnected':
      return 'Disconnected';
  }
}
