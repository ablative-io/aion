import { useSyncExternalStore } from 'react';

import type { AionSocketError, ConnectionStatus } from '@/lib/api';
import { configuredEventSocket } from '@/lib/config';

export function useConnectionStatus(): ConnectionStatus {
  return useSyncExternalStore(
    (notify) => configuredEventSocket.onStatusChange(notify),
    () => configuredEventSocket.getStatus(),
    () => 'disconnected'
  );
}

/**
 * Reads the live socket's last typed error (M1). Returns `null` when the stream
 * is healthy. Consumers render this as visible state instead of letting socket
 * failures vanish into the console.
 */
export function useSocketError(): AionSocketError | null {
  return useSyncExternalStore(
    (notify) => configuredEventSocket.onError(() => notify()),
    () => configuredEventSocket.getLastError(),
    () => null
  );
}
