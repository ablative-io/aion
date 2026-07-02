import type { KeyboardEvent, RefObject } from 'react';
import { useCallback, useEffect, useId, useRef, useState } from 'react';

// Shared machinery for the trigger→surface morphs (MorphingPopover,
// FloatingPanel): open-state control, click-outside + Escape dismissal, and
// focus return to the trigger. Non-blocking by design — no backdrop, the rest
// of the console stays interactive.

export type AnchoredMorphOptions = {
  open?: boolean;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
};

export type AnchoredMorph = {
  open: boolean;
  setOpen: (open: boolean) => void;
  /** Shared layoutId linking trigger and surface into one continuous morph. */
  layoutId: string;
  rootRef: RefObject<HTMLDivElement | null>;
  triggerRef: RefObject<HTMLButtonElement | null>;
  contentRef: RefObject<HTMLDivElement | null>;
  /**
   * Element-scoped Escape handling for the open surface (focus is inside it);
   * global bindings belong to the central keybinding registry, not here.
   */
  onContentKeyDown: (event: KeyboardEvent<HTMLDivElement>) => void;
};

export function useAnchoredMorph({
  open,
  defaultOpen = false,
  onOpenChange,
}: AnchoredMorphOptions): AnchoredMorph {
  const layoutId = useId();
  const [internalOpen, setInternalOpen] = useState(defaultOpen);
  const isOpen = open ?? internalOpen;

  const rootRef = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const contentRef = useRef<HTMLDivElement | null>(null);

  const isControlled = open !== undefined;
  const setOpen = useCallback(
    (next: boolean) => {
      if (!isControlled) setInternalOpen(next);
      onOpenChange?.(next);
      if (!next) triggerRef.current?.focus();
    },
    [isControlled, onOpenChange]
  );

  // Dismiss on pointer-down outside the whole trigger+surface cluster.
  useEffect(() => {
    if (!isOpen) return;
    const onPointerDown = (event: PointerEvent) => {
      const root = rootRef.current;
      if (root && event.target instanceof Node && !root.contains(event.target)) {
        setOpen(false);
      }
    };
    document.addEventListener('pointerdown', onPointerDown);
    return () => document.removeEventListener('pointerdown', onPointerDown);
  }, [isOpen, setOpen]);

  // Move focus into the surface once it opens so Escape lands on it.
  useEffect(() => {
    if (isOpen) contentRef.current?.focus();
  }, [isOpen]);

  const onContentKeyDown = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      if (event.key === 'Escape') {
        event.stopPropagation();
        setOpen(false);
      }
    },
    [setOpen]
  );

  return { open: isOpen, setOpen, layoutId, rootRef, triggerRef, contentRef, onContentKeyDown };
}
