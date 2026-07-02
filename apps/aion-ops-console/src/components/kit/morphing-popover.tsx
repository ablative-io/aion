import { AnimatePresence, motion, useReducedMotion } from 'motion/react';
import type { ReactNode } from 'react';
import { createContext, useContext } from 'react';

import { cn } from '@/lib/utils';
import type { AnchoredMorph } from './anchored-morph';
import { useAnchoredMorph } from './anchored-morph';
import { degradeToFade, MICRO_TRANSITION, SPRING_SIGNATURE } from './springs';

// Trigger→surface shared-layoutId morph (motion-primitives pattern, MIT,
// reimplemented in our idiom): the trigger IS the surface, expanded in place.
// Anchored at the trigger, non-blocking (no backdrop), Escape/click-outside
// dismiss, focus returns to the trigger on close.

const MorphingPopoverContext = createContext<AnchoredMorph | null>(null);

function usePopoverContext(component: string): AnchoredMorph {
  const context = useContext(MorphingPopoverContext);
  if (!context) throw new Error(`${component} must be used inside <MorphingPopover>`);
  return context;
}

export type MorphingPopoverProps = {
  children: ReactNode;
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  className?: string;
};

export function MorphingPopover({
  children,
  open,
  defaultOpen,
  onOpenChange,
  className,
}: MorphingPopoverProps) {
  const morph = useAnchoredMorph({
    ...(open !== undefined ? { open } : {}),
    ...(defaultOpen !== undefined ? { defaultOpen } : {}),
    ...(onOpenChange ? { onOpenChange } : {}),
  });

  return (
    <MorphingPopoverContext.Provider value={morph}>
      <div
        ref={morph.rootRef}
        className={cn('relative inline-block', className)}
        data-slot="morphing-popover"
        data-open={morph.open}
      >
        {children}
      </div>
    </MorphingPopoverContext.Provider>
  );
}

export function MorphingPopoverTrigger({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  const morph = usePopoverContext('MorphingPopoverTrigger');
  const reducedMotion = useReducedMotion() ?? false;

  return (
    <AnimatePresence initial={false}>
      {!morph.open && (
        <motion.button
          type="button"
          ref={morph.triggerRef}
          {...(reducedMotion ? {} : { layoutId: morph.layoutId })}
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={MICRO_TRANSITION}
          onClick={() => morph.setOpen(true)}
          className={cn(
            'inline-flex items-center gap-2 rounded-lg border border-[var(--border-default)]',
            'bg-[var(--surface-elevated,#16161d)] px-3 py-1.5 text-xs text-[var(--text-secondary)]',
            'transition-colors duration-150 hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]',
            'outline-none focus-visible:ring-2 focus-visible:ring-[var(--border-focus)]',
            className
          )}
          data-slot="morphing-popover-trigger"
        >
          {children}
        </motion.button>
      )}
    </AnimatePresence>
  );
}

export function MorphingPopoverContent({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  const morph = usePopoverContext('MorphingPopoverContent');
  const reducedMotion = useReducedMotion() ?? false;
  const transition = degradeToFade(SPRING_SIGNATURE, reducedMotion);

  return (
    <AnimatePresence initial={false}>
      {morph.open && (
        <motion.div
          ref={morph.contentRef}
          {...(reducedMotion ? {} : { layoutId: morph.layoutId })}
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0, filter: 'blur(10px)' }}
          transition={transition}
          tabIndex={-1}
          role="dialog"
          onKeyDown={morph.onContentKeyDown}
          className={cn(
            // Glass is sanctioned for summoned surfaces (design language §tokens).
            'absolute left-0 top-0 z-50 min-w-56 rounded-xl border border-[var(--border-default)]',
            'bg-[var(--surface-glass,oklch(0.16_0.01_280/0.85))] p-3 shadow-xl backdrop-blur-xl',
            'outline-none',
            className
          )}
          data-slot="morphing-popover-content"
        >
          {children}
        </motion.div>
      )}
    </AnimatePresence>
  );
}
