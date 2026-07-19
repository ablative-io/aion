import type {
  AionSocketError,
  ManagedWebSocket,
  SocketErrorListener,
  SubscriptionRecord,
  TimeoutHandle,
} from './websocket-types';
import { SOCKET_CONNECTING } from './websocket-types';

export type SubscriptionSocketState = 'idle' | 'connected' | 'reconnecting' | 'exhausted';

export type SubscriptionConnection = {
  subscription: SubscriptionRecord;
  socket: ManagedWebSocket | null;
  state: SubscriptionSocketState;
  reconnectAttempts: number;
  pendingMessages: unknown[];
  reconnectTimer: TimeoutHandle | null;
  error: AionSocketError | null;
  errorSequence: number;
};

/** Maintains one error per logical connection and projects the newest active one. */
export class SubscriptionErrorState {
  private lastError: AionSocketError | null = null;
  private nextErrorSequence = 1;

  constructor(
    private readonly connections: ReadonlyMap<string, SubscriptionConnection>,
    private readonly listeners: ReadonlySet<SocketErrorListener>
  ) {}

  getLastError(): AionSocketError | null {
    return this.lastError;
  }

  set(connection: SubscriptionConnection, error: AionSocketError): void {
    connection.error = error;
    connection.errorSequence = this.nextErrorSequence;
    this.nextErrorSequence += 1;
    this.refresh();
  }

  clear(connection: SubscriptionConnection): void {
    if (connection.error === null) {
      return;
    }

    connection.error = null;
    connection.errorSequence = 0;
    this.refresh();
  }

  refresh(): void {
    let latestConnection: SubscriptionConnection | null = null;

    for (const connection of this.connections.values()) {
      if (
        connection.error !== null &&
        (latestConnection === null || connection.errorSequence > latestConnection.errorSequence)
      ) {
        latestConnection = connection;
      }
    }

    const nextError = latestConnection?.error ?? null;
    if (this.lastError === nextError) {
      return;
    }

    this.lastError = nextError;
    for (const listener of this.listeners) {
      listener(nextError);
    }
  }

  reset(): void {
    this.lastError = null;
    this.nextErrorSequence = 1;
  }
}

export function isCurrentConnection(
  connections: ReadonlyMap<string, SubscriptionConnection>,
  connection: SubscriptionConnection,
  socket: ManagedWebSocket
): boolean {
  return connections.get(connection.subscription.id) === connection && connection.socket === socket;
}

export function drainPendingMessages(
  connection: SubscriptionConnection,
  socket: ManagedWebSocket,
  handleMessage: (data: unknown) => void
): boolean {
  const pendingMessages = connection.pendingMessages;
  connection.pendingMessages = [];

  for (const data of pendingMessages) {
    handleMessage(data);
    if (connection.socket !== socket) {
      return false;
    }
  }

  return true;
}

export function failFeedBoundary(socket: ManagedWebSocket, disconnect: () => void): void {
  // Disconnect first: nulling the manager's current socket synchronously makes
  // queued messages stale before close dispatches.
  disconnect();
  closeWhenSafe(socket);
}

/**
 * Close a socket without ever calling `close()` while it is still CONNECTING.
 *
 * Browsers throw `DOMException: WebSocket is closed before the connection is
 * established` (logged as a console error) when `close()` is invoked on a socket
 * in the CONNECTING state — which happens under React StrictMode's double-mount,
 * where the cleanup runs before the just-opened socket has finished connecting.
 *
 * To tear down cleanly we defer the close to the `onopen` transition, then close
 * once the socket is OPEN. We overwrite the socket's listeners with no-op/closing
 * handlers so the abandoned socket can neither dispatch frames nor trigger the
 * manager's reconnect logic. If the connection never opens (it errors/closes
 * first), the deferred close is a harmless no-op on an already-closed socket.
 */
export function closeWhenSafe(socket: ManagedWebSocket): void {
  if (socket.readyState === SOCKET_CONNECTING) {
    socket.onmessage = null;
    socket.onclose = null;
    socket.onerror = null;
    socket.onopen = () => {
      socket.close();
    };
    return;
  }

  socket.close();
}
