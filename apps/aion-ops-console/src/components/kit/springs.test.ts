import { describe, expect, test } from 'bun:test';

import {
  degradeToFade,
  MICRO_TRANSITION,
  MICRO_TRANSITION_SLOW,
  REDUCED_MOTION_FADE,
  SPRING_SECONDARY,
  SPRING_SIGNATURE,
  SPRING_SUCCESS,
} from './springs';

describe('house physics constants', () => {
  test('the signature spring carries the ratified constants', () => {
    expect(SPRING_SIGNATURE).toEqual({ type: 'spring', stiffness: 550, damping: 45, mass: 0.7 });
    expect(SPRING_SECONDARY).toEqual({ type: 'spring', stiffness: 350, damping: 35 });
    expect(SPRING_SUCCESS).toEqual({ type: 'spring', stiffness: 500, damping: 22 });
  });

  test('micro-transitions stay on the CSS timescale (150-200ms ease-out)', () => {
    for (const micro of [MICRO_TRANSITION, MICRO_TRANSITION_SLOW]) {
      expect(micro.ease).toBe('easeOut');
      expect(micro.duration).toBeGreaterThanOrEqual(0.15);
      expect(micro.duration).toBeLessThanOrEqual(0.2);
    }
  });
});

describe('reduced-motion degradation', () => {
  test('springs degrade to the opacity-fade timing under reduced motion', () => {
    for (const spring of [SPRING_SIGNATURE, SPRING_SECONDARY, SPRING_SUCCESS]) {
      const degraded = degradeToFade(spring, true);
      expect(degraded).toEqual(REDUCED_MOTION_FADE);
      expect('type' in degraded && degraded.type === 'spring').toBe(false);
    }
  });

  test('springs pass through untouched when motion is allowed', () => {
    expect(degradeToFade(SPRING_SIGNATURE, false)).toBe(SPRING_SIGNATURE);
    expect(degradeToFade(SPRING_SECONDARY, false)).toBe(SPRING_SECONDARY);
  });

  test('micro tweens are already motionless and pass through under reduced motion', () => {
    expect(degradeToFade(MICRO_TRANSITION, true)).toBe(MICRO_TRANSITION);
  });
});
