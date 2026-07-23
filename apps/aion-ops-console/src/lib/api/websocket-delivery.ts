import {
  failFeedBoundary,
  type SubscriptionConnection,
  type SubscriptionErrorState,
} from './websocket-connection';
import {
  assertExpectedWorkflowSequence,
  frameDecodeError,
  parseFrame,
  subscriberApplicationError,
} from './websocket-protocol';
import type { ApplicationRecoveryPolicy } from './websocket-recovery-policy';
import type { ManagedWebSocket, WarningLogger } from './websocket-types';

export function flushPendingDelivery(
  connection: SubscriptionConnection,
  warn: WarningLogger,
  connectionErrors: SubscriptionErrorState
): void {
  try {
    connection.subscription.flushPending?.();
  } catch (error) {
    warn('Aion event subscriber failed to flush buffered WebSocket frames', error);
    connectionErrors.set(connection, subscriberApplicationError(error, connection.subscription.id));
  }
}

/** Parse, validate, and apply one frame while the manager retains socket ownership. */
export function deliverEventMessage(
  connection: SubscriptionConnection,
  socket: ManagedWebSocket,
  data: unknown,
  dependencies: {
    warn: WarningLogger;
    connectionErrors: SubscriptionErrorState;
    recoveryPolicy: ApplicationRecoveryPolicy;
    disconnect: () => void;
  }
): void {
  const { warn, connectionErrors, recoveryPolicy, disconnect } = dependencies;
  let frame: ReturnType<typeof parseFrame>;

  try {
    frame = parseFrame(data);
    assertExpectedWorkflowSequence(connection.subscription, frame.event);
  } catch (error) {
    flushPendingDelivery(connection, warn, connectionErrors);
    warn('Unable to parse Aion event WebSocket frame', error);
    connectionErrors.set(connection, frameDecodeError(error, connection.subscription.id));
    failFeedBoundary(socket, disconnect);
    return;
  }

  const subscription = connection.subscription;
  let recoveryImpact: ReturnType<typeof subscription.handler>;
  try {
    recoveryImpact = subscription.handler(frame.event, {
      subscriptionId: subscription.id,
      namespace: frame.namespace,
      filter: subscription.filter,
    });
  } catch (error) {
    warn('Aion event subscriber failed to apply a WebSocket frame', error);
    connectionErrors.set(connection, subscriberApplicationError(error, subscription.id));
    failFeedBoundary(socket, disconnect);
    return;
  }

  // The durable cursor means "already applied", not merely decoded/delivered.
  recoveryPolicy.markFrameDelivered(connection, socket, recoveryImpact);
  subscription.lastSeenSequence = frame.event.data.envelope.seq;
  if (recoveryPolicy.isUnresolved(connection.error) && subscription.filter.kind === 'workflow') {
    recoveryPolicy.complete(connection, socket);
  }
}
