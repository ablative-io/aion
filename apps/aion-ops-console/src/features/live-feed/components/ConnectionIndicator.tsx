import { Wifi, WifiOff } from 'lucide-react';

import { Badge } from '@/components/ui';
import type { AionSocketError, ConnectionStatus } from '@/lib/api';
import { cn } from '@/lib/utils';

import { useConnectionStatus, useSocketError } from '../hooks/useConnectionStatus';

const STATUS_LABELS: Record<ConnectionStatus, string> = {
  connected: 'Connected',
  disconnected: 'Disconnected',
  reconnecting: 'Reconnecting',
};

const STATUS_STYLES: Record<ConnectionStatus, string> = {
  connected: 'border-emerald-500/40 bg-emerald-500/10 text-emerald-500',
  disconnected: 'border-destructive/40 bg-destructive/10 text-destructive',
  reconnecting: 'border-amber-500/40 bg-amber-500/10 text-amber-500',
};

export type ConnectionIndicatorContentProps = {
  status: ConnectionStatus;
  className?: string | undefined;
  /**
   * Last typed socket error (M1). When present it is rendered as visible state
   * below the badge so a stranger can read *why* the stream is degraded.
   */
  error?: AionSocketError | null | undefined;
};

/**
 * Presentational connection indicator. Renders a given status with no hook
 * dependency, so it can be driven by a parent that already owns the status
 * (e.g. the firehose header) or unit-tested directly.
 */
export function ConnectionIndicatorContent({
  status,
  className,
  error = null,
}: ConnectionIndicatorContentProps) {
  const Icon = status === 'connected' ? Wifi : WifiOff;

  return (
    <div className={cn('flex min-w-40 flex-col gap-2', className)} data-connection-status={status}>
      <span className="text-muted-foreground text-xs font-medium">Event stream</span>
      <Badge
        aria-label={`WebSocket ${STATUS_LABELS[status].toLowerCase()}`}
        className={cn('w-fit gap-2', STATUS_STYLES[status])}
        variant="outline"
      >
        <Icon aria-hidden="true" className="size-3.5" />
        {STATUS_LABELS[status]}
      </Badge>
      {error !== null ? (
        <p className="text-destructive text-xs" data-socket-error={error.kind} role="alert">
          {error.message}
        </p>
      ) : null}
    </div>
  );
}

export type ConnectionIndicatorProps = {
  className?: string;
};

/**
 * Live connection indicator wired to the shared WebSocket status. Surfaces
 * drop/reconnect transitions to visible state (never console-only).
 */
export function ConnectionIndicator({ className }: ConnectionIndicatorProps) {
  const status = useConnectionStatus();
  const error = useSocketError();

  return <ConnectionIndicatorContent className={className} error={error} status={status} />;
}
