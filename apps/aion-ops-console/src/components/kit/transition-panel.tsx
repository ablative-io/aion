import { AnimatePresence, motion, useReducedMotion } from 'motion/react';
import type { ReactNode } from 'react';
import { useRef } from 'react';

import { cn } from '@/lib/utils';

import { degradeToFade, SPRING_SIGNATURE } from './springs';

// TransitionPanel (motion-primitives pattern): a directional view switch —
// the outgoing panel slides toward where you came from, the incoming one
// arrives from where you're going, so tab order reads spatially.

export type TransitionPanelProps = {
  activeIndex: number;
  children: ReactNode[];
  className?: string;
  /** Slide distance in px; direction is derived from index movement. */
  distance?: number;
};

export function TransitionPanel({
  activeIndex,
  children,
  className,
  distance = 24,
}: TransitionPanelProps) {
  const reducedMotion = useReducedMotion() ?? false;
  const transition = degradeToFade(SPRING_SIGNATURE, reducedMotion);

  // Derive travel direction from the previous index without re-rendering.
  const previousIndexRef = useRef(activeIndex);
  const direction = activeIndex >= previousIndexRef.current ? 1 : -1;
  previousIndexRef.current = activeIndex;

  const travel = reducedMotion ? 0 : distance;

  return (
    <div className={cn('relative overflow-hidden', className)} data-slot="transition-panel">
      <AnimatePresence custom={direction} initial={false} mode="popLayout">
        <motion.div
          key={activeIndex}
          custom={direction}
          variants={{
            enter: (dir: number) => ({ opacity: 0, x: dir * travel }),
            center: { opacity: 1, x: 0 },
            exit: (dir: number) => ({ opacity: 0, x: dir * -travel, filter: 'blur(10px)' }),
          }}
          initial="enter"
          animate="center"
          exit="exit"
          transition={transition}
          data-slot="transition-panel-view"
        >
          {children[activeIndex]}
        </motion.div>
      </AnimatePresence>
    </div>
  );
}
