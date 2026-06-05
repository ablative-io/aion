import { describe, expect, mock, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { ConnectionStatus } from '@/lib/api';

let currentStatus: ConnectionStatus = 'disconnected';

mock.module('../hooks/useConnectionStatus', () => ({
  useConnectionStatus: () => currentStatus,
}));

describe('ConnectionIndicator', () => {
  test('renders distinct connected, reconnecting, and disconnected states', async () => {
    const { ConnectionIndicator } = await import('./ConnectionIndicator');
    const statuses = ['connected', 'reconnecting', 'disconnected'] satisfies ConnectionStatus[];
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
});

function connectionLabel(status: ConnectionStatus): string {
  switch (status) {
    case 'connected':
      return 'Connected';
    case 'reconnecting':
      return 'Reconnecting';
    case 'disconnected':
      return 'Disconnected';
  }
}
