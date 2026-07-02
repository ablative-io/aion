import { cn } from '@/lib/utils';

// The one way status is communicated (design language: "small colored dot +
// glow-tinted chip, tokenized — never hardcoded per-component again"). Token
// names follow the semantic status set; values live only in index.css.
export type KitStatus = 'healthy' | 'running' | 'failed' | 'special' | 'live' | 'idle';

export const KIT_STATUS_COLOR: Record<KitStatus, string> = {
  healthy: 'var(--status-success)',
  running: 'var(--status-warning)',
  failed: 'var(--status-danger)',
  special: 'var(--status-special)',
  live: 'var(--status-live)',
  idle: 'var(--text-muted)',
};

export const KIT_STATUS_GLOW: Record<KitStatus, string> = {
  healthy: 'var(--status-success-glow)',
  running: 'var(--status-warning-glow)',
  failed: 'var(--status-danger-glow)',
  special: 'var(--status-special-glow)',
  live: 'var(--status-live-glow)',
  idle: 'var(--surface-hover)',
};

/** The primary accent — act/selection/focus. Never cyan. */
export const KIT_ACCENT = 'var(--accent-primary)';
export const KIT_ACCENT_GLOW = 'var(--accent-primary-glow)';

export type StatusDotProps = {
  status: KitStatus;
  /** Pulse is reserved for live/streaming semantics. */
  pulse?: boolean;
  className?: string;
};

export function StatusDot({ status, pulse = false, className }: StatusDotProps) {
  return (
    <span
      aria-hidden="true"
      className={cn('relative inline-flex size-2 shrink-0 rounded-full', className)}
      data-slot="status-dot"
      data-status={status}
      style={{
        backgroundColor: KIT_STATUS_COLOR[status],
        boxShadow: `0 0 0 3px ${KIT_STATUS_GLOW[status]}`,
      }}
    >
      {pulse ? (
        <span
          className="absolute inset-0 animate-ping rounded-full opacity-60 motion-reduce:hidden"
          style={{ backgroundColor: KIT_STATUS_COLOR[status] }}
        />
      ) : null}
    </span>
  );
}
