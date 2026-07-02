import { ChevronRight } from 'lucide-react';
import { motion, useReducedMotion } from 'motion/react';
import type { ReactNode } from 'react';
import { useState } from 'react';
import useMeasure from 'react-use-measure';

import { cn } from '@/lib/utils';

import { degradeToFade, MICRO_TRANSITION, SPRING_SIGNATURE } from './springs';
import type { KitStatus } from './status-dot';
import { StatusDot } from './status-dot';

// Expandable row (cult-ui Expandable pattern): a one-line summary — icon
// slot, text, status dot, optional pips — spring-expanding to measured-height
// detail. Controlled (expanded/onExpandedChange) or uncontrolled
// (defaultExpanded); the per-kind transcript renderers plug into `children`.

/** Which expansion state wins: the controlled prop when present, else internal. */
export function resolveExpanded(controlled: boolean | undefined, internal: boolean): boolean {
  return controlled ?? internal;
}

export type ExpandableRowProps = {
  expanded?: boolean;
  defaultExpanded?: boolean;
  onExpandedChange?: (expanded: boolean) => void;
  icon?: ReactNode;
  summary: ReactNode;
  status?: KitStatus;
  /** Small progress pips (e.g. attempt count); rendered dim, filled up to `pipsFilled`. */
  pips?: number;
  pipsFilled?: number;
  children: ReactNode;
  className?: string;
};

export function ExpandableRow({
  expanded,
  defaultExpanded = false,
  onExpandedChange,
  icon,
  summary,
  status,
  pips,
  pipsFilled = 0,
  children,
  className,
}: ExpandableRowProps) {
  const reducedMotion = useReducedMotion() ?? false;
  const transition = degradeToFade(SPRING_SIGNATURE, reducedMotion);
  const [internalExpanded, setInternalExpanded] = useState(defaultExpanded);
  const isExpanded = resolveExpanded(expanded, internalExpanded);
  const [detailRef, detailBounds] = useMeasure();

  const toggle = () => {
    const next = !isExpanded;
    if (expanded === undefined) setInternalExpanded(next);
    onExpandedChange?.(next);
  };

  return (
    <div
      className={cn(
        'rounded-lg border border-[var(--border-subtle,rgba(255,255,255,0.04))]',
        'bg-[var(--surface-elevated,#16161d)]',
        className
      )}
      data-slot="expandable-row"
      data-expanded={isExpanded}
    >
      <button
        type="button"
        onClick={toggle}
        aria-expanded={isExpanded}
        className="flex w-full items-center gap-2 rounded-lg px-3 py-2 text-left transition-colors duration-150 hover:bg-[var(--surface-hover)] outline-none focus-visible:ring-2 focus-visible:ring-[var(--border-focus)]"
      >
        <motion.span
          animate={{ rotate: isExpanded ? 90 : 0 }}
          transition={MICRO_TRANSITION}
          className="shrink-0 text-[var(--text-muted)]"
        >
          <ChevronRight aria-hidden="true" className="size-3.5" />
        </motion.span>
        {icon ? <span className="shrink-0 text-[var(--text-secondary)]">{icon}</span> : null}
        <span className="min-w-0 flex-1 truncate text-sm text-[var(--text-primary)]">
          {summary}
        </span>
        {typeof pips === 'number' && pips > 0 ? (
          <span className="flex shrink-0 items-center gap-1" data-slot="expandable-row-pips">
            {Array.from({ length: pips }, (_, pip) => (
              <span
                // biome-ignore lint/suspicious/noArrayIndexKey: pips are positional by nature
                key={pip}
                className={cn(
                  'size-1.5 rounded-full',
                  pip < pipsFilled
                    ? 'bg-[var(--accent-primary,#6b96d1)]'
                    : 'bg-[var(--surface-hover)]'
                )}
              />
            ))}
          </span>
        ) : null}
        {status ? <StatusDot status={status} pulse={status === 'live'} /> : null}
      </button>
      <motion.div
        animate={{ height: isExpanded ? detailBounds.height : 0 }}
        transition={transition}
        className="overflow-hidden"
        initial={false}
        data-slot="expandable-row-detail"
        aria-hidden={!isExpanded}
      >
        <div
          ref={detailRef}
          className="border-t border-[var(--border-subtle,rgba(255,255,255,0.04))] px-3 py-2.5 text-sm text-[var(--text-secondary)]"
        >
          {children}
        </div>
      </motion.div>
    </div>
  );
}
