import { cn } from '@/lib/utils';

// The one way status is communicated (design language: "small colored dot +
// glow-tinted chip, tokenized — never hardcoded per-component again"). Token
// names follow the semantic status set; hex fallbacks are the doc's ratified
// values so the kit renders correctly even before the token pass lands.
export type KitStatus = 'healthy' | 'running' | 'failed' | 'special' | 'live' | 'idle';

export const KIT_STATUS_COLOR: Record<KitStatus, string> = {
  healthy: 'var(--status-success, #74b584)',
  running: 'var(--status-warning, #fbbf24)',
  failed: 'var(--status-danger, #f87171)',
  special: 'var(--status-special, #a78bfa)',
  live: 'var(--status-live, #4fb8b2)',
  idle: 'var(--text-muted, #71717a)',
};

export const KIT_STATUS_GLOW: Record<KitStatus, string> = {
  healthy: 'var(--status-success-glow, rgba(116,181,132,0.12))',
  running: 'var(--status-warning-glow, rgba(251,191,36,0.12))',
  failed: 'var(--status-danger-glow, rgba(248,113,113,0.12))',
  special: 'var(--status-special-glow, rgba(167,139,250,0.12))',
  live: 'var(--status-live-glow, rgba(79,184,178,0.12))',
  idle: 'var(--surface-hover, #252530)',
};

/** The action accent (brand strategy-blue) — act/selection/focus. Never cyan. */
export const KIT_ACCENT = 'var(--accent-primary, #6b96d1)';
export const KIT_ACCENT_GLOW = 'var(--accent-primary-glow, rgba(107,150,209,0.12))';

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
