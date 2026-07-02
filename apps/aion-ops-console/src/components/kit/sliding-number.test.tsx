import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import { SlidingNumber, splitSlidingNumber } from './sliding-number';

describe('splitSlidingNumber', () => {
  test('splits an integer into per-place digit columns', () => {
    expect(splitSlidingNumber(407)).toEqual({ sign: '', integer: ['4', '0', '7'], fraction: [] });
  });

  test('splits fractional values around the separator', () => {
    expect(splitSlidingNumber(12.5)).toEqual({ sign: '', integer: ['1', '2'], fraction: ['5'] });
    expect(splitSlidingNumber(0.25)).toEqual({ sign: '', integer: ['0'], fraction: ['2', '5'] });
  });

  test('carries the sign separately so only digits slide', () => {
    expect(splitSlidingNumber(-31)).toEqual({ sign: '-', integer: ['3', '1'], fraction: [] });
  });

  test('pads the integer side for clock-style displays', () => {
    expect(splitSlidingNumber(7, 2).integer).toEqual(['0', '7']);
    expect(splitSlidingNumber(0, 3).integer).toEqual(['0', '0', '0']);
    // Padding never truncates a wider value.
    expect(splitSlidingNumber(1234, 2).integer).toEqual(['1', '2', '3', '4']);
  });

  test('zero renders a single column', () => {
    expect(splitSlidingNumber(0)).toEqual({ sign: '', integer: ['0'], fraction: [] });
  });
});

describe('SlidingNumber rendering', () => {
  test('renders one odometer column per digit plus the separator', () => {
    const markup = renderToStaticMarkup(<SlidingNumber value={12.5} />);
    const columns = markup.match(/data-slot="sliding-digit"/g) ?? [];
    expect(columns.length).toBe(3);
    expect(markup).toContain('.');
    expect(markup).toContain('data-slot="sliding-number"');
  });

  test('each animated column is keyed to its digit', () => {
    const markup = renderToStaticMarkup(<SlidingNumber value={42} />);
    expect(markup).toContain('data-digit="4"');
    expect(markup).toContain('data-digit="2"');
  });
});
