import { useSyncExternalStore } from 'react';

import { aionEventSocket, type ConnectionStatus } from '@/lib/api';

export function useConnectionStatus(): ConnectionStatus {
  return useSyncExternalStore(
    (notify) => aionEventSocket.onStatusChange(notify),
    () => aionEventSocket.getStatus(),
    () => 'disconnected'
  );
}
