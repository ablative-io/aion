import type { Transition } from 'motion/react';
import { useReducedMotion } from 'motion/react';

// The house physics module (OPS-CONSOLE-DESIGN-LANGUAGE.md "Motion vocabulary").
// Every kit component takes its transitions from here — no component invents
// its own spring constants.

/** The signature spring: surface morphs (island expand, panel summon, entity forms). */
export const SPRING_SIGNATURE: Transition = {
  type: 'spring',
  stiffness: 550,
  damping: 45,
  mass: 0.7,
};

/** Secondary elements: rows, chips, digits — anything inside a surface. */
export const SPRING_SECONDARY: Transition = {
  type: 'spring',
  stiffness: 350,
  damping: 35,
};

/** Success/confirm: a touch bouncy, for completed states and confirmations. */
export const SPRING_SUCCESS: Transition = {
  type: 'spring',
  stiffness: 500,
  damping: 22,
};

/** Micro-transitions (hover, chevrons): CSS-timescale ease-out, 150ms. */
export const MICRO_TRANSITION: Transition = {
  duration: 0.15,
  ease: 'easeOut',
};

/** Slower micro for exits paired with fade + blur, 200ms. */
export const MICRO_TRANSITION_SLOW: Transition = {
  duration: 0.2,
  ease: 'easeOut',
};

/** What any spring degrades to under prefers-reduced-motion: a plain opacity fade. */
export const REDUCED_MOTION_FADE: Transition = {
  duration: 0.2,
  ease: 'easeOut',
};

/**
 * Pure form of the reduced-motion degrade so it is testable without a
 * renderer: springs collapse to the fade timing; already-micro tweens pass
 * through unchanged (they carry no motion, only timing).
 */
export function degradeToFade(transition: Transition, reducedMotion: boolean): Transition {
  if (!reducedMotion) return transition;
  if (!('type' in transition) || transition.type !== 'spring') return transition;
  return REDUCED_MOTION_FADE;
}

/**
 * Take a house transition and degrade it to an opacity fade when the operator
 * prefers reduced motion. Components that also *move* things (layout morphs)
 * must additionally disable their layout animation via useReducedMotion —
 * this hook handles the timing curve, not the choreography.
 */
export function useReducedMotionTransition(transition: Transition): Transition {
  const reducedMotion = useReducedMotion();
  return degradeToFade(transition, reducedMotion ?? false);
}
