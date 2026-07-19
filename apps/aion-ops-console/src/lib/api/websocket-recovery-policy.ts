import type { SubscriptionConnection, SubscriptionErrorState } from './websocket-connection';
import { buildResyncContext, subscriberResyncError } from './websocket-protocol';
import type {
  AionSocketError,
  ManagedWebSocket,
  Scheduler,
  TimeoutHandle,
  WarningLogger,
} from './websocket-types';

type RecoveryHost = {
  isCurrent(connection: SubscriptionConnection, socket: ManagedWebSocket): boolean;
  updateStatus(): void;
  failBoundary(connection: SubscriptionConnection, socket: ManagedWebSocket): void;
};

/** Coordinates application-level recovery independently of socket handshakes. */
export class ApplicationRecoveryPolicy {
  constructor(
    private readonly warn: WarningLogger,
    private readonly errors: SubscriptionErrorState,
    private readonly host: RecoveryHost,
    private readonly scheduler: Scheduler,
    private readonly resyncTimeoutMs: number
  ) {}

  isUnresolved(error: AionSocketError | null): boolean {
    return error?.kind === 'frame-decode' || error?.kind === 'subscriber-application';
  }

  recoverLiveOnly(connection: SubscriptionConnection, socket: ManagedWebSocket): void {
    const onResync = connection.subscription.onResync;

    if (onResync === undefined) {
      // There is no recovery claim to make. The live socket remains usable, but
      // its possible-gap state and any boundary error stay visible indefinitely.
      return;
    }

    const timeout = this.scheduler.setTimeout(() => {
      if (connection.resyncTimer !== timeout) {
        return;
      }

      connection.resyncTimer = null;
      this.fail(
        connection,
        socket,
        new Error(`Live-state refetch timed out after ${this.resyncTimeoutMs}ms`)
      );
    }, this.resyncTimeoutMs);
    connection.resyncTimer = timeout;

    let recovery: void | Promise<void>;

    try {
      recovery = onResync(buildResyncContext(connection.subscription));
    } catch (error) {
      this.clearTimeout(connection, timeout);
      this.fail(connection, socket, error);
      return;
    }

    void Promise.resolve(recovery).then(
      () => {
        this.clearTimeout(connection, timeout);
        this.complete(connection, socket);
      },
      (error: unknown) => {
        this.clearTimeout(connection, timeout);
        this.fail(connection, socket, error);
      }
    );
  }

  cancel(connection: SubscriptionConnection): void {
    const timeout = connection.resyncTimer;
    if (timeout !== null) {
      this.clearTimeout(connection, timeout);
    }
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

  private clearTimeout(connection: SubscriptionConnection, timeout: TimeoutHandle): void {
    if (connection.resyncTimer !== timeout) {
      return;
    }

    connection.resyncTimer = null;
    this.scheduler.clearTimeout(timeout);
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
