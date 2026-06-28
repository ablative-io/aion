import { useSyncExternalStore } from 'react';

import { type AionSocketError, aionEventSocket, type ConnectionStatus } from '@/lib/api';

export function useConnectionStatus(): ConnectionStatus {
  return useSyncExternalStore(
    (notify) => aionEventSocket.onStatusChange(notify),
    () => aionEventSocket.getStatus(),
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
    (notify) => aionEventSocket.onError(() => notify()),
    () => aionEventSocket.getLastError(),
    () => null
  );
}
