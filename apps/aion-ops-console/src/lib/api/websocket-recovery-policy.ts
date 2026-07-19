import type { SubscriptionConnection, SubscriptionErrorState } from './websocket-connection';
import { buildResyncContext, subscriberResyncError } from './websocket-protocol';
import type { AionSocketError, ManagedWebSocket, WarningLogger } from './websocket-types';

type RecoveryHost = {
  isCurrent(connection: SubscriptionConnection, socket: ManagedWebSocket): boolean;
  updateStatus(): void;
  drainPending(connection: SubscriptionConnection, socket: ManagedWebSocket): boolean;
  failBoundary(connection: SubscriptionConnection, socket: ManagedWebSocket): void;
};

/** Coordinates application-level recovery independently of socket handshakes. */
export class ApplicationRecoveryPolicy {
  constructor(
    private readonly warn: WarningLogger,
    private readonly errors: SubscriptionErrorState,
    private readonly host: RecoveryHost
  ) {}

  isUnresolved(error: AionSocketError | null): boolean {
    return error?.kind === 'frame-decode' || error?.kind === 'subscriber-application';
  }

  recoverLiveOnly(connection: SubscriptionConnection, socket: ManagedWebSocket): void {
    const onResync = connection.subscription.onResync;

    if (onResync === undefined) {
      this.fail(
        connection,
        socket,
        new Error('Live-only subscriptions require an onResync full-refetch callback')
      );
      return;
    }

    let recovery: void | Promise<void>;

    try {
      recovery = onResync(buildResyncContext(connection.subscription));
    } catch (error) {
      this.fail(connection, socket, error);
      return;
    }

    void Promise.resolve(recovery).then(
      () => this.completeLiveOnly(connection, socket),
      (error: unknown) => this.fail(connection, socket, error)
    );
  }

  complete(connection: SubscriptionConnection, socket: ManagedWebSocket): void {
    if (!this.host.isCurrent(connection, socket)) {
      return;
    }

    connection.reconnectAttempts = 0;
    connection.state = 'connected';
    this.errors.clear(connection);
    this.host.updateStatus();
  }

  private completeLiveOnly(connection: SubscriptionConnection, socket: ManagedWebSocket): void {
    if (this.host.isCurrent(connection, socket) && this.host.drainPending(connection, socket)) {
      this.complete(connection, socket);
    }
  }

  notifyDurable(connection: SubscriptionConnection): void {
    const onResync = connection.subscription.onResync;

    if (onResync === undefined) {
      return;
    }

    try {
      void Promise.resolve(onResync(buildResyncContext(connection.subscription))).catch((error) => {
        this.warn('Aion workflow subscriber resync notification failed', error);
      });
    } catch (error) {
      this.warn('Aion workflow subscriber resync notification failed', error);
    }
  }

  private fail(connection: SubscriptionConnection, socket: ManagedWebSocket, error: unknown): void {
    if (!this.host.isCurrent(connection, socket)) {
      return;
    }

    this.warn('Aion event subscriber failed to resynchronize after reconnecting', error);
    this.errors.set(connection, subscriberResyncError(error, connection.subscription.id));
    this.host.failBoundary(connection, socket);
  }
}
