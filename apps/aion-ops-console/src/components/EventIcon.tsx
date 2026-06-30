import {
  Activity,
  Bell,
  CheckCircle2,
  CircleDot,
  CircleSlash,
  GitBranch,
  type LucideIcon,
  Timer,
  XCircle,
} from 'lucide-react';

import { cn } from '@/lib/utils';

export type EventIconKind = 'lifecycle' | 'activity' | 'timer' | 'signal' | 'child' | 'generic';

export type EventIconTone = 'neutral' | 'success' | 'danger' | 'warning' | 'info';

type EventIconProps = {
  kind: EventIconKind;
  tone?: EventIconTone;
};

type EventIconMetadata = {
  label: string;
  className: string;
  Icon: LucideIcon;
};

const toneClasses: Record<EventIconTone, string> = {
  neutral: 'bg-[var(--surface-hover)] text-[var(--text-muted)]',
  success: 'bg-emerald-500/15 text-emerald-400',
  danger: 'bg-red-500/15 text-red-400',
  warning: 'bg-amber-500/15 text-amber-400',
  info: 'bg-sky-500/15 text-sky-400',
};

const EVENT_ICON_METADATA: Record<EventIconKind, EventIconMetadata> = {
  lifecycle: {
    label: 'Workflow lifecycle event',
    className: 'ring-emerald-400/20',
    Icon: CheckCircle2,
  },
  activity: {
    label: 'Activity event',
    className: 'ring-violet-400/20',
    Icon: Activity,
  },
  timer: {
    label: 'Timer event',
    className: 'ring-sky-400/20',
    Icon: Timer,
  },
  signal: {
    label: 'Signal event',
    className: 'ring-fuchsia-400/20',
    Icon: Bell,
  },
  child: {
    label: 'Child workflow event',
    className: 'ring-cyan-400/20',
    Icon: GitBranch,
  },
  generic: {
    label: 'Workflow event',
    className: 'ring-zinc-400/20',
    Icon: CircleDot,
  },
};

function EventIcon({ kind, tone = 'neutral' }: EventIconProps) {
  const metadata = iconMetadataForKind(kind, tone);
  const Icon = metadata.Icon;

  return (
    <span
      aria-label={metadata.label}
      className={cn(
        'inline-flex size-10 items-center justify-center rounded-full ring-1',
        toneClasses[tone],
        metadata.className
      )}
      data-event-kind={kind}
      data-event-tone={tone}
      role="img"
    >
      <Icon aria-hidden="true" className="size-5" />
    </span>
  );
}

function iconMetadataForKind(kind: EventIconKind, tone: EventIconTone): EventIconMetadata {
  if (kind === 'lifecycle') {
    if (tone === 'success') {
      return { ...EVENT_ICON_METADATA.lifecycle, Icon: CheckCircle2 };
    }

    if (tone === 'danger') {
      return { ...EVENT_ICON_METADATA.lifecycle, Icon: XCircle };
    }

    if (tone === 'warning') {
      return { ...EVENT_ICON_METADATA.lifecycle, Icon: CircleSlash };
    }
  }

  return EVENT_ICON_METADATA[kind];
}

export { EVENT_ICON_METADATA, EventIcon, iconMetadataForKind };
