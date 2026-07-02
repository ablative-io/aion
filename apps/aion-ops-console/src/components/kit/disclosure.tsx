import { ChevronDown } from 'lucide-react';
import { motion, useReducedMotion } from 'motion/react';
import type { ReactNode } from 'react';
import { createContext, useContext, useId, useState } from 'react';
import useMeasure from 'react-use-measure';

import { cn } from '@/lib/utils';

import { degradeToFade, MICRO_TRANSITION, SPRING_SIGNATURE } from './springs';

// Disclosure (motion-primitives pattern): an animated collapsible for detail
// panels — measured-height expansion on the signature spring, chevron folded
// into the trigger. Controlled or uncontrolled.

type DisclosureContextValue = {
  open: boolean;
  toggle: () => void;
  contentId: string;
};

const DisclosureContext = createContext<DisclosureContextValue | null>(null);

function useDisclosureContext(component: string): DisclosureContextValue {
  const context = useContext(DisclosureContext);
  if (!context) throw new Error(`${component} must be used inside <Disclosure>`);
  return context;
}

export type DisclosureProps = {
  children: ReactNode;
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  className?: string;
};

export function Disclosure({
  children,
  open,
  defaultOpen = false,
  onOpenChange,
  className,
}: DisclosureProps) {
  const contentId = useId();
  const [internalOpen, setInternalOpen] = useState(defaultOpen);
  const isOpen = open ?? internalOpen;

  const toggle = () => {
    const next = !isOpen;
    if (open === undefined) setInternalOpen(next);
    onOpenChange?.(next);
  };

  return (
    <DisclosureContext.Provider value={{ open: isOpen, toggle, contentId }}>
      <div className={cn('flex flex-col', className)} data-slot="disclosure" data-open={isOpen}>
        {children}
      </div>
    </DisclosureContext.Provider>
  );
}

export function DisclosureTrigger({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  const disclosure = useDisclosureContext('DisclosureTrigger');

  return (
    <button
      type="button"
      onClick={disclosure.toggle}
      aria-expanded={disclosure.open}
      aria-controls={disclosure.contentId}
      className={cn(
        'flex items-center justify-between gap-2 rounded-lg px-1 py-1.5 text-left text-sm',
        'text-[var(--text-primary)] transition-colors duration-150 hover:text-[var(--text-secondary)]',
        'outline-none focus-visible:ring-2 focus-visible:ring-[var(--border-focus)]',
        className
      )}
      data-slot="disclosure-trigger"
    >
      {children}
      <motion.span
        animate={{ rotate: disclosure.open ? 180 : 0 }}
        transition={MICRO_TRANSITION}
        className="shrink-0 text-[var(--text-muted)]"
      >
        <ChevronDown aria-hidden="true" className="size-3.5" />
      </motion.span>
    </button>
  );
}

export function DisclosureContent({
  children,
  className,
}: {
  children: ReactNode;
  className?: string;
}) {
  const disclosure = useDisclosureContext('DisclosureContent');
  const reducedMotion = useReducedMotion() ?? false;
  const transition = degradeToFade(SPRING_SIGNATURE, reducedMotion);
  const [contentRef, bounds] = useMeasure();

  return (
    <motion.div
      id={disclosure.contentId}
      animate={{
        height: disclosure.open ? bounds.height : 0,
        opacity: disclosure.open ? 1 : 0,
      }}
      transition={transition}
      initial={false}
      className="overflow-hidden"
      aria-hidden={!disclosure.open}
      data-slot="disclosure-content"
    >
      <div ref={contentRef} className={cn('pt-1 text-sm text-[var(--text-secondary)]', className)}>
        {children}
      </div>
    </motion.div>
  );
}
