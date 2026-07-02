import { motion, useReducedMotion } from 'motion/react';

import { cn } from '@/lib/utils';

import { degradeToFade, SPRING_SECONDARY } from './springs';

/**
 * SlidingNumber — a per-digit odometer for live values.
 *
 * Bling guard (Tom's rule): use ONLY where the number IS the information —
 * live durations, running counts, streaming metrics. Static figures just
 * render as text; no gratuitous gauges.
 */

export type SlidingNumberParts = {
  sign: '' | '-';
  integer: string[];
  fraction: string[];
};

/** Pure digit split so the odometer's column layout is directly testable. */
export function splitSlidingNumber(value: number, padStart = 1): SlidingNumberParts {
  const sign: SlidingNumberParts['sign'] = value < 0 ? '-' : '';
  const absolute = Math.abs(value);
  // toFixed-free split keeps whatever precision the caller passed in.
  const [rawInteger = '0', rawFraction = ''] = absolute.toString().split('.');
  const integer = rawInteger.padStart(Math.max(padStart, 1), '0').split('');
  const fraction = rawFraction.split('').filter((char) => char !== '');
  return { sign, integer, fraction };
}

const DIGITS = ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];

export type SlidingNumberProps = {
  value: number;
  /** Minimum integer digit count, zero-padded (clocks: padStart={2}). */
  padStart?: number;
  className?: string;
};

export function SlidingNumber({ value, padStart = 1, className }: SlidingNumberProps) {
  const reducedMotion = useReducedMotion() ?? false;
  const { sign, integer, fraction } = splitSlidingNumber(value, padStart);

  return (
    <span
      className={cn('mono inline-flex items-baseline tabular-nums', className)}
      data-slot="sliding-number"
    >
      {sign === '-' ? <span>-</span> : null}
      {integer.map((digit, place) => (
        <SlidingDigit
          // biome-ignore lint/suspicious/noArrayIndexKey: a digit column IS its place value
          key={`int-${integer.length - place}`}
          digit={digit}
          reducedMotion={reducedMotion}
        />
      ))}
      {fraction.length > 0 ? <span>.</span> : null}
      {fraction.map((digit, place) => (
        <SlidingDigit
          // biome-ignore lint/suspicious/noArrayIndexKey: a digit column IS its place value
          key={`frac-${place}`}
          digit={digit}
          reducedMotion={reducedMotion}
        />
      ))}
    </span>
  );
}

function SlidingDigit({ digit, reducedMotion }: { digit: string; reducedMotion: boolean }) {
  // Non-numeric characters (exponent notation edge) render statically.
  const numeric = DIGITS.includes(digit);
  if (reducedMotion || !numeric) {
    return <span data-slot="sliding-digit">{digit}</span>;
  }

  return (
    <span
      className="relative inline-block h-[1em] w-[1ch] overflow-hidden align-baseline"
      data-slot="sliding-digit"
      data-digit={digit}
    >
      <motion.span
        className="absolute left-0 top-0 flex flex-col"
        animate={{ y: `-${Number(digit)}em` }}
        transition={degradeToFade(SPRING_SECONDARY, false)}
        initial={false}
      >
        {DIGITS.map((column) => (
          <span key={column} className="flex h-[1em] items-center justify-center leading-none">
            {column}
          </span>
        ))}
      </motion.span>
    </span>
  );
}
