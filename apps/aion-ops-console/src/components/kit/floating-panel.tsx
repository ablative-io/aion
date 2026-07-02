import { X } from 'lucide-react';
import { AnimatePresence, motion, useReducedMotion } from 'motion/react';
import type { ReactNode } from 'react';
import { createContext, useContext } from 'react';

import { cn } from '@/lib/utils';
import type { AnchoredMorph } from './anchored-morph';
import { useAnchoredMorph } from './anchored-morph';
import { degradeToFade, SPRING_SIGNATURE } from './springs';

// FloatingPanel (cult-ui pattern, MIT, reimplemented in our idiom): a richer
// anchored morph than MorphingPopover — a structured mini-surface (header /
// body / footer) for small workflows like deploy forms or payload inspectors.
// Same rules: anchored at trigger, non-blocking, Escape/click-outside
// dismiss, focus return.

type FloatingPanelContextValue = AnchoredMorph & { title: string };

const FloatingPanelContext = createContext<FloatingPanelContextValue | null>(null);

function usePanelContext(component: string): FloatingPanelContextValue {
  const context = useContext(FloatingPanelContext);
  if (!context) throw new Error(`${component} must be used inside <FloatingPanelRoot>`);
  return context;
}

export type FloatingPanelRootProps = {
  children: ReactNode;
  /** Names the panel for assistive tech and the header. */
  title: string;
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  className?: string;
};

export function FloatingPanelRoot({
  children,
  title,
  open,
  defaultOpen,
  onOpenChange,
  className,
}: FloatingPanelRootProps) {
  const morph = useAnchoredMorph({
    ...(open !== undefined ? { open } : {}),
    ...(defaultOpen !== undefined ? { defaultOpen } : {}),
    ...(onOpenChange ? { onOpenChange } : {}),
  });

  return (
    <FloatingPanelContext.Provider value={{ ...morph, title }}>
      <div
        ref={morph.rootRef}
        className={cn('relative inline-block', className)}
        data-slot="floating-panel"
        data-open={morph.open}
      >
        {children}
      </div>
    </FloatingPanelContext.Provider>
  );
}

export function FloatingPanelTrigger({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  const panel = usePanelContext('FloatingPanelTrigger');
  const reducedMotion = useReducedMotion() ?? false;

  return (
    <AnimatePresence initial={false}>
      {!panel.open && (
        <motion.button
          type="button"
          ref={panel.triggerRef}
          {...(reducedMotion ? {} : { layoutId: panel.layoutId })}
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.15, ease: 'easeOut' }}
          onClick={() => panel.setOpen(true)}
          aria-haspopup="dialog"
          className={cn(
            'inline-flex items-center gap-2 rounded-lg border border-[var(--border-default)]',
            'bg-[var(--surface-elevated,#16161d)] px-3 py-1.5 text-xs text-[var(--text-secondary)]',
            'transition-colors duration-150 hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]',
            'outline-none focus-visible:ring-2 focus-visible:ring-[var(--border-focus)]',
            className
          )}
          data-slot="floating-panel-trigger"
        >
          {children}
        </motion.button>
      )}
    </AnimatePresence>
  );
}

export function FloatingPanelContent({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  const panel = usePanelContext('FloatingPanelContent');
  const reducedMotion = useReducedMotion() ?? false;
  const transition = degradeToFade(SPRING_SIGNATURE, reducedMotion);

  return (
    <AnimatePresence initial={false}>
      {panel.open && (
        <motion.div
          ref={panel.contentRef}
          {...(reducedMotion ? {} : { layoutId: panel.layoutId })}
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0, filter: 'blur(10px)' }}
          transition={transition}
          tabIndex={-1}
          role="dialog"
          aria-label={panel.title}
          onKeyDown={panel.onContentKeyDown}
          className={cn(
            'absolute left-0 top-0 z-50 flex w-80 flex-col overflow-hidden rounded-xl',
            'border border-[var(--border-default)] shadow-xl backdrop-blur-xl',
            'bg-[var(--surface-glass,oklch(0.16_0.01_280/0.85))] outline-none',
            className
          )}
          data-slot="floating-panel-content"
        >
          <div className="flex items-center justify-between border-b border-[var(--border-subtle,rgba(255,255,255,0.04))] px-4 py-2.5">
            <span className="text-xs font-semibold uppercase tracking-[0.15em] text-[var(--text-secondary)]">
              {panel.title}
            </span>
            <FloatingPanelClose />
          </div>
          {children}
        </motion.div>
      )}
    </AnimatePresence>
  );
}

export function FloatingPanelBody({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn('px-4 py-3 text-sm text-[var(--text-primary)]', className)}>{children}</div>
  );
}

export function FloatingPanelFooter({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        'flex items-center justify-end gap-2 border-t px-4 py-2.5',
        'border-[var(--border-subtle,rgba(255,255,255,0.04))]',
        className
      )}
    >
      {children}
    </div>
  );
}

function FloatingPanelClose() {
  const panel = usePanelContext('FloatingPanelClose');

  return (
    <button
      type="button"
      aria-label={`Close ${panel.title}`}
      onClick={() => panel.setOpen(false)}
      className="rounded-md p-1 text-[var(--text-muted)] transition-colors duration-150 hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)] outline-none focus-visible:ring-2 focus-visible:ring-[var(--border-focus)]"
    >
      <X aria-hidden="true" className="size-3.5" />
    </button>
  );
}
