import { X } from 'lucide-react';
import { AnimatePresence, motion, useReducedMotion } from 'motion/react';
import type { KitStatus } from '@/components/kit';
import { degradeToFade, SPRING_SIGNATURE, StatusDot } from '@/components/kit';
import { Badge, Button } from '@/components/ui';

import { DetailPanelBody } from '../components/DetailPanel';
import type {
  ActivityTimelineEntry,
  ChildWorkflowTimelineEntry,
  LifecycleTimelineEntry,
  TimelineEntry,
  TimerTimelineEntry,
} from '../types';

type DetailSheetProps = {
  /** The selected entry, or `null` to dismiss the sheet. */
  entry: TimelineEntry | null;
  /**
   * The clicked bar's horizontal centre, in px from the timeline container's left
   * edge. The sheet morphs out of that x-origin so the bar reads as "the entity"
   * that opened. `null` (e.g. a back/forward restore, or list mode) morphs from
   * the horizontal centre.
   */
  originX: number | null;
  onClose: () => void;
};

/**
 * The bottom-docked, morphing detail sheet (PART 2). It replaces the old
 * right-side `DetailPanel` that COMPRESSED the full-width timeline: the Gantt keeps
 * its full width and this sheet docks BELOW it with its own scroll axis. The sheet
 * MORPHS out of the clicked bar's x-origin (an origin transform driven by
 * `originX`), landing docked, so it reads as "this thing opened," not "a panel
 * appeared." The header is an entity-pill — the collapsed identity of what is
 * selected (summary + status dot + seq) — and it is non-blocking, summon-and-
 * dismiss: dismissing clears the selection. Honors `prefers-reduced-motion`.
 */
function DetailSheet({ entry, originX, onClose }: DetailSheetProps) {
  const reducedMotion = useReducedMotion() ?? false;
  const transition = degradeToFade(SPRING_SIGNATURE, reducedMotion);
  const transformOrigin = originX === null ? 'center top' : `${originX}px top`;

  return (
    <AnimatePresence initial={false}>
      {entry === null ? null : (
        <motion.section
          aria-label={`Event detail for seq ${entry.envelope.seq}`}
          className="overflow-hidden rounded-xl border border-border bg-surface-elevated"
          data-testid="detail-sheet"
          exit={reducedMotion ? { opacity: 0 } : { opacity: 0, scaleY: 0.7, scaleX: 0.92, y: -6 }}
          initial={
            reducedMotion ? { opacity: 0 } : { opacity: 0, scaleY: 0.7, scaleX: 0.92, y: -6 }
          }
          animate={{ opacity: 1, scaleY: 1, scaleX: 1, y: 0 }}
          style={{ transformOrigin }}
          transition={transition}
        >
          <SheetHeader entry={entry} onClose={onClose} />
          <div className="max-h-[42vh] overflow-auto px-4 py-4" data-testid="detail-sheet-scroll">
            <DetailPanelBody entry={entry} />
          </div>
        </motion.section>
      )}
    </AnimatePresence>
  );
}

/** The entity-pill header: collapsed identity of the selected entry + dismiss. */
function SheetHeader({ entry, onClose }: { entry: TimelineEntry; onClose: () => void }) {
  return (
    <header className="flex items-start justify-between gap-3 border-border border-b bg-surface-base px-4 py-3">
      <div className="flex min-w-0 items-center gap-2">
        <StatusDot pulse={entryKitStatus(entry) === 'live'} status={entryKitStatus(entry)} />
        <Badge variant="secondary">{entry.kind}</Badge>
        <Badge variant="outline">seq {entry.envelope.seq}</Badge>
        <h2 className="min-w-0 truncate font-medium text-foreground text-sm">{entry.summary}</h2>
      </div>
      <Button
        aria-label="Close detail sheet"
        className="size-8 shrink-0 p-0"
        onClick={onClose}
        type="button"
        variant="ghost"
      >
        <X aria-hidden="true" className="size-4" />
      </Button>
    </header>
  );
}

/** Map a timeline entry's derived status onto the tokenized kit status vocabulary. */
function entryKitStatus(entry: TimelineEntry): KitStatus {
  switch (entry.kind) {
    case 'lifecycle':
      return lifecycleKitStatus(entry);
    case 'activity':
      return activityKitStatus(entry);
    case 'timer':
      return timerKitStatus(entry);
    case 'child':
      return childKitStatus(entry);
    default:
      return 'idle';
  }
}

function lifecycleKitStatus(entry: LifecycleTimelineEntry): KitStatus {
  switch (entry.outcome) {
    case 'completed':
      return 'healthy';
    case 'failed':
    case 'timed-out':
      return 'failed';
    case 'cancelled':
      return 'special';
    default:
      return 'running';
  }
}

function activityKitStatus(entry: ActivityTimelineEntry): KitStatus {
  switch (entry.status) {
    case 'completed':
      return 'healthy';
    case 'failed':
      return 'failed';
    case 'cancelled':
      return 'special';
    case 'started':
      return 'running';
    default:
      return 'idle';
  }
}

function timerKitStatus(entry: TimerTimelineEntry): KitStatus {
  switch (entry.status) {
    case 'completed':
    case 'fired':
      return 'healthy';
    case 'timed-out':
      return 'failed';
    case 'cancelled':
      return 'special';
    default:
      return 'running';
  }
}

function childKitStatus(entry: ChildWorkflowTimelineEntry): KitStatus {
  switch (entry.status) {
    case 'completed':
      return 'healthy';
    case 'failed':
      return 'failed';
    case 'cancelled':
      return 'special';
    default:
      return 'running';
  }
}

export { DetailSheet };
