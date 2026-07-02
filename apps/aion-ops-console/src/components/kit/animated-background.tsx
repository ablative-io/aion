import { AnimatePresence, motion, useReducedMotion } from 'motion/react';
import type { ReactElement, ReactNode } from 'react';
import { Children, cloneElement, useId, useState } from 'react';

import { cn } from '@/lib/utils';

import { degradeToFade, SPRING_SIGNATURE } from './springs';

// AnimatedBackground (motion-primitives pattern, MIT, reimplemented): a
// shared-layout highlight that slides between item rows / nav tabs /
// segmented-control options instead of blinking on and off.

export type AnimatedBackgroundItemProps = {
  'data-id': string;
  children?: ReactNode;
  className?: string;
  onClick?: () => void;
};

export type AnimatedBackgroundProps = {
  /** Items must carry a unique `data-id`; the highlight tracks the active one. */
  children: ReactElement<AnimatedBackgroundItemProps> | ReactElement<AnimatedBackgroundItemProps>[];
  defaultValue?: string;
  onValueChange?: (activeId: string | null) => void;
  /** Class applied to the sliding highlight surface itself. */
  highlightClassName?: string;
  /** Follow hover instead of selection (nav affordances). */
  enableHover?: boolean;
  className?: string;
};

export function AnimatedBackground({
  children,
  defaultValue,
  onValueChange,
  highlightClassName,
  enableHover = false,
  className,
}: AnimatedBackgroundProps) {
  const layoutGroupId = useId();
  const reducedMotion = useReducedMotion() ?? false;
  const transition = degradeToFade(SPRING_SIGNATURE, reducedMotion);
  const [activeId, setActiveId] = useState<string | null>(defaultValue ?? null);

  const activate = (id: string | null) => {
    setActiveId(id);
    onValueChange?.(id);
  };

  return Children.map(children, (child) => {
    const id = child.props['data-id'];
    const interactionProps = enableHover
      ? {
          onMouseEnter: () => activate(id),
          onMouseLeave: () => activate(null),
        }
      : {
          onClick: () => {
            child.props.onClick?.();
            activate(id);
          },
        };

    // data-checked is not modeled by the item's prop type, so widen the clone
    // props to a string-keyed record (still no `any`).
    const cloneProps: Partial<AnimatedBackgroundItemProps> & Record<string, unknown> = {
      ...interactionProps,
      className: cn('relative', className, child.props.className),
      'data-checked': activeId === id ? 'true' : 'false',
    };

    return cloneElement(
      child,
      cloneProps,
      <>
        <AnimatePresence initial={false}>
          {activeId === id && (
            <motion.div
              {...(reducedMotion ? {} : { layoutId: `animated-background-${layoutGroupId}` })}
              transition={transition}
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0 }}
              className={cn(
                'absolute inset-0 rounded-lg bg-[var(--surface-hover)]',
                highlightClassName
              )}
              data-slot="animated-background-highlight"
            />
          )}
        </AnimatePresence>
        <span className="relative z-10">{child.props.children}</span>
      </>
    );
  });
}
