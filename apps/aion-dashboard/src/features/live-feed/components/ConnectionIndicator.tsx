import { Wifi, WifiOff } from 'lucide-react';

import { Badge } from '@/components/ui';
import type { ConnectionStatus } from '@/lib/api';
import { cn } from '@/lib/utils';

import { useConnectionStatus } from '../hooks/useConnectionStatus';

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

export type ConnectionIndicatorProps = {
  className?: string;
};

export function ConnectionIndicator({ className }: ConnectionIndicatorProps) {
  const status = useConnectionStatus();
  const Icon = status === 'connected' ? Wifi : WifiOff;

  return (
    <div className={cn('flex min-w-40 flex-col gap-2', className)}>
      <span className="text-muted-foreground text-xs font-medium">Event stream</span>
      <Badge
        aria-label={`WebSocket ${STATUS_LABELS[status].toLowerCase()}`}
        className={cn('w-fit gap-2', STATUS_STYLES[status])}
        variant="outline"
      >
        <Icon aria-hidden="true" className="size-3.5" />
        {STATUS_LABELS[status]}
      </Badge>
    </div>
  );
}
