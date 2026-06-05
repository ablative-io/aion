import { Cloud, CloudOff, type LucideIcon, RefreshCcw } from 'lucide-react';

import type { ConnectionStatus } from '@/lib/api';
import { cn } from '@/lib/utils';
import { useConnectionStatus } from '../hooks/useConnectionStatus';

type ConnectionIndicatorMetadata = {
  label: string;
  className: string;
  dotClassName: string;
  Icon: LucideIcon;
};

const CONNECTION_INDICATOR_METADATA: Record<ConnectionStatus, ConnectionIndicatorMetadata> = {
  connected: {
    label: 'Live stream connected',
    className: 'border-emerald-400/30 bg-emerald-500/10 text-emerald-300',
    dotClassName: 'bg-emerald-300',
    Icon: Cloud,
  },
  reconnecting: {
    label: 'Live stream reconnecting',
    className: 'border-amber-400/30 bg-amber-500/10 text-amber-300',
    dotClassName: 'bg-amber-300 animate-pulse',
    Icon: RefreshCcw,
  },
  disconnected: {
    label: 'Live stream disconnected',
    className: 'border-red-400/30 bg-red-500/10 text-red-300',
    dotClassName: 'bg-red-300',
    Icon: CloudOff,
  },
};

function ConnectionIndicator() {
  const status = useConnectionStatus();
  const metadata = CONNECTION_INDICATOR_METADATA[status];
  const Icon = metadata.Icon;

  return (
    <div
      aria-label={metadata.label}
      className={cn(
        'inline-flex items-center gap-2 rounded-full border px-3 py-1 font-medium text-xs',
        metadata.className
      )}
      data-connection-status={status}
      role="status"
    >
      <span className={cn('size-2 rounded-full', metadata.dotClassName)} />
      <Icon aria-hidden="true" className="size-3.5" />
      <span>{statusLabel(status)}</span>
    </div>
  );
}

function statusLabel(status: ConnectionStatus): string {
  switch (status) {
    case 'connected':
      return 'Connected';
    case 'reconnecting':
      return 'Reconnecting';
    case 'disconnected':
      return 'Disconnected';
  }
}

export { CONNECTION_INDICATOR_METADATA, ConnectionIndicator, statusLabel };
