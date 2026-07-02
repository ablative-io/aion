import { describe, expect, test } from 'bun:test';

import { computeVirtualWindow } from '../components/TranscriptList';

describe('computeVirtualWindow', () => {
  test('windows the rendered rows to the viewport plus overscan', () => {
    // 100 rows of 50px in a 200px viewport, scrolled to row 40 (2000px).
    const heights = new Array<number>(100).fill(50);
    const range = computeVirtualWindow(heights, 2000, 200);

    // Visible rows are 40..44; overscan of 8 extends to 32..52.
    expect(range.start).toBe(32);
    expect(range.end).toBe(52);
    // Spacers stand in for exactly the unrendered rows on each side.
    expect(range.topPad).toBe(32 * 50);
    expect(range.bottomPad).toBe((100 - 52) * 50);
  });

  test('renders everything when the list fits the viewport', () => {
    const heights = [40, 60, 55];
    const range = computeVirtualWindow(heights, 0, 500);
    expect(range).toEqual({ start: 0, end: 3, topPad: 0, bottomPad: 0 });
  });

  test('respects variable measured heights when placing the window', () => {
    // First row is huge: scrolled past it, the window starts at the next rows.
    const heights = [1000, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50];
    const range = computeVirtualWindow(heights, 1100, 100);
    // Visible rows are indices 2..4; overscan clamps start to 0.
    expect(range.start).toBe(0);
    expect(range.end).toBe(12);
  });

  test('before the viewport is measured it renders the tail so content shows', () => {
    const heights = new Array<number>(500).fill(50);
    const range = computeVirtualWindow(heights, 0, 0);
    expect(range.end).toBe(500);
    expect(range.start).toBe(300);
    expect(range.topPad).toBe(300 * 50);
    expect(range.bottomPad).toBe(0);
  });
});
