import type {
  RecoveryAttempt,
  SubscriptionConnection,
  SubscriptionErrorState,
} from './websocket-connection';
import {
  buildResyncContext,
  reconnectExhaustedError,
  subscriberResyncError,
} from './websocket-protocol';
import type {
  AionSocketError,
  ManagedWebSocket,
  RecoveryFrameImpact,
  Scheduler,
  WarningLogger,
} from './websocket-types';

type RecoveryHost = {
  isCurrent(connection: SubscriptionConnection, socket: ManagedWebSocket): boolean;
  updateStatus(): void;
  failBoundary(connection: SubscriptionConnection, socket: ManagedWebSocket): void;
  exhaustBoundary(connection: SubscriptionConnection, socket: ManagedWebSocket): void;
};

/** Coordinates application-level recovery independently of socket handshakes. */
export class ApplicationRecoveryPolicy {
  constructor(
    private readonly warn: WarningLogger,
    private readonly errors: SubscriptionErrorState,
    private readonly host: RecoveryHost,
    private readonly scheduler: Scheduler,
    private readonly resyncTimeoutMs: number,
    private readonly maxAttempts: number
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

    const attempt = this.beginAttempt(connection, socket, 'live-only');
    attempt.timer = this.scheduler.setTimeout(() => {
      if (!this.isCurrentAttempt(connection, attempt)) {
        return;
      }

      this.finishAttempt(connection, attempt);
      this.fail(
        connection,
        socket,
        new Error(`Live-state refetch timed out after ${this.resyncTimeoutMs}ms`)
      );
    }, this.resyncTimeoutMs);

    let recovery: void | Promise<void>;

    try {
      recovery = onResync(this.contextFor(connection, attempt));
    } catch (error) {
      this.finishAttempt(connection, attempt);
      this.fail(connection, socket, error);
      return;
    }

    void Promise.resolve(recovery).then(
      () => {
        if (!this.isCurrentAttempt(connection, attempt)) {
          return;
        }

        const wasDirty = attempt.dirty;
        this.finishAttempt(connection, attempt);

        if (wasDirty) {
          // A snapshot request that overlapped a delivered frame cannot establish
          // which result was newer. Spend the same bounded recovery budget and
          // repeat until one full refetch observes a quiescent live interval.
          if (this.beginRecoveryRetry(connection, socket)) {
            this.recoverLiveOnly(connection, socket);
          }
          return;
        }

        this.complete(connection, socket);
      },
      (error: unknown) => {
        if (!this.isCurrentAttempt(connection, attempt)) {
          return;
        }

        this.finishAttempt(connection, attempt);
        this.fail(connection, socket, error);
      }
    );
  }

  /** Marks a relevant applied frame as racing an in-flight full refetch. */
  markFrameDelivered(
    connection: SubscriptionConnection,
    socket: ManagedWebSocket,
    impact: RecoveryFrameImpact
  ): void {
    const attempt = connection.recoveryAttempt;
    if (
      impact !== false &&
      attempt !== null &&
      attempt.kind === 'live-only' &&
      attempt.socket === socket &&
      this.isCurrentAttempt(connection, attempt)
    ) {
      attempt.dirty = true;
    }
  }

  cancel(connection: SubscriptionConnection): void {
    const attempt = connection.recoveryAttempt;
    if (attempt !== null) {
      this.finishAttempt(connection, attempt);
    }
  }

  complete(connection: SubscriptionConnection, socket: ManagedWebSocket): void {
    if (!this.host.isCurrent(connection, socket)) {
      return;
    }

    // A durable replay frame is stronger completion evidence than its optional
    // refetch notification. Invalidate any still-running notification generation.
    this.cancel(connection);
    connection.reconnectAttempts = 0;
    connection.state = 'connected';
    this.errors.clear(connection);
    this.host.updateStatus();
  }

  notifyDurable(connection: SubscriptionConnection, socket: ManagedWebSocket): void {
    const onResync = connection.subscription.onResync;

    if (onResync === undefined) {
      return;
    }

    const attempt = this.beginAttempt(connection, socket, 'durable-notification');
    let notification: void | Promise<void>;

    try {
      notification = onResync(this.contextFor(connection, attempt));
    } catch (error) {
      this.finishAttempt(connection, attempt);
      this.warn('Aion workflow subscriber resync notification failed', error);
      return;
    }

    void Promise.resolve(notification).then(
      () => {
        if (this.isCurrentAttempt(connection, attempt)) {
          this.finishAttempt(connection, attempt);
        }
      },
      (error: unknown) => {
        if (!this.isCurrentAttempt(connection, attempt)) {
          return;
        }

        this.finishAttempt(connection, attempt);
        this.warn('Aion workflow subscriber resync notification failed', error);
      }
    );
  }

  private beginAttempt(
    connection: SubscriptionConnection,
    socket: ManagedWebSocket,
    kind: RecoveryAttempt['kind']
  ): RecoveryAttempt {
    this.cancel(connection);
    connection.recoveryGeneration += 1;
    const attempt: RecoveryAttempt = {
      generation: connection.recoveryGeneration,
      socket,
      controller: new AbortController(),
      dirty: false,
      kind,
      timer: null,
    };
    connection.recoveryAttempt = attempt;
    return attempt;
  }

  private contextFor(connection: SubscriptionConnection, attempt: RecoveryAttempt) {
    return buildResyncContext(connection.subscription, {
      generation: attempt.generation,
      signal: attempt.controller.signal,
      isCurrent: () => this.isCurrentAttempt(connection, attempt),
    });
  }

  private isCurrentAttempt(connection: SubscriptionConnection, attempt: RecoveryAttempt): boolean {
    return (
      connection.recoveryAttempt === attempt &&
      !attempt.controller.signal.aborted &&
      this.host.isCurrent(connection, attempt.socket)
    );
  }

  private finishAttempt(connection: SubscriptionConnection, attempt: RecoveryAttempt): void {
    if (connection.recoveryAttempt !== attempt) {
      return;
    }

    connection.recoveryAttempt = null;
    if (attempt.timer !== null) {
      this.scheduler.clearTimeout(attempt.timer);
      attempt.timer = null;
    }
    attempt.controller.abort();
  }

  private beginRecoveryRetry(
    connection: SubscriptionConnection,
    socket: ManagedWebSocket
  ): boolean {
    if (connection.reconnectAttempts >= this.maxAttempts) {
      connection.state = 'exhausted';
      this.errors.set(
        connection,
        reconnectExhaustedError(this.maxAttempts, connection.subscription.id)
      );
      this.host.exhaustBoundary(connection, socket);
      return false;
    }

    connection.reconnectAttempts += 1;
    return true;
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
