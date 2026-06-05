import { Activity, Bell, CheckCircle2, CircleSlash, GitBranch, Timer, XCircle } from 'lucide-react';

import { cn } from '@/lib/utils';

type EventIconKind = 'lifecycle' | 'activity' | 'timer' | 'signal' | 'child';

type EventIconTone = 'neutral' | 'success' | 'danger' | 'warning' | 'info';

type EventIconProps = {
  kind: EventIconKind;
  tone?: EventIconTone;
};

const toneClasses: Record<EventIconTone, string> = {
  neutral: 'bg-[var(--surface-hover)] text-[var(--text-muted)]',
  success: 'bg-emerald-500/15 text-emerald-400',
  danger: 'bg-red-500/15 text-red-400',
  warning: 'bg-amber-500/15 text-amber-400',
  info: 'bg-sky-500/15 text-sky-400',
};

function EventIcon({ kind, tone = 'neutral' }: EventIconProps) {
  const Icon = iconForKind(kind, tone);

  return (
    <span
      aria-hidden="true"
      className={cn(
        'inline-flex size-10 items-center justify-center rounded-full',
        toneClasses[tone]
      )}
    >
      <Icon className="size-5" />
    </span>
  );
}

function iconForKind(kind: EventIconKind, tone: EventIconTone) {
  if (kind === 'lifecycle') {
    if (tone === 'success') {
      return CheckCircle2;
    }

    if (tone === 'danger') {
      return XCircle;
    }

    if (tone === 'warning') {
      return CircleSlash;
    }
  }

  switch (kind) {
    case 'activity':
      return Activity;
    case 'timer':
      return Timer;
    case 'signal':
      return Bell;
    case 'child':
      return GitBranch;
    case 'lifecycle':
      return CheckCircle2;
  }
}

export type { EventIconKind, EventIconTone };
export { EventIcon };
