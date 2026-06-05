import type { ConnectionStatus } from '@/lib/api';

import { useConnectionStatus } from '../hooks/useConnectionStatus';

const STATUS_LABELS = {
  connected: 'Live updates connected',
  disconnected: 'Live updates disconnected',
  reconnecting: 'Live updates reconnecting',
} as const;

const STATUS_CLASSES = {
  connected: 'border-emerald-500/30 bg-emerald-500/10 text-emerald-700',
  disconnected: 'border-slate-500/30 bg-slate-500/10 text-slate-700',
  reconnecting: 'border-amber-500/30 bg-amber-500/10 text-amber-700',
} as const;

export function ConnectionIndicator() {
  return <ConnectionIndicatorContent status={useConnectionStatus()} />;
}

export function ConnectionIndicatorContent({ status }: { status: ConnectionStatus }) {
  return (
    <div
      aria-live="polite"
      className={`inline-flex items-center gap-2 rounded-full border px-3 py-1 text-sm ${STATUS_CLASSES[status]}`}
      data-status={status}
    >
      <span className="h-2 w-2 rounded-full bg-current" />
      {STATUS_LABELS[status]}
    </div>
  );
}
