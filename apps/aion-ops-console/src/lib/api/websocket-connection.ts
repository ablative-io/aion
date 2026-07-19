import type {
  AionSocketError,
  ManagedWebSocket,
  SocketErrorListener,
  SubscriptionRecord,
  TimeoutHandle,
} from './websocket-types';

export type SubscriptionSocketState = 'idle' | 'connected' | 'reconnecting' | 'exhausted';

export type SubscriptionConnection = {
  subscription: SubscriptionRecord;
  socket: ManagedWebSocket | null;
  state: SubscriptionSocketState;
  reconnectAttempts: number;
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
