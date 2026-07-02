import { describe, expect, test } from 'bun:test';
import { blendOver, contrastRatio, formatRatio, parseCssColor } from './contrast';

describe('parseCssColor', () => {
  test('parses 6-digit hex', () => {
    expect(parseCssColor('#6b96d1')).toEqual({ r: 107, g: 150, b: 209, a: 1 });
  });

  test('parses 3-digit hex', () => {
    expect(parseCssColor('#fff')).toEqual({ r: 255, g: 255, b: 255, a: 1 });
  });

  test('parses rgba() with alpha', () => {
    expect(parseCssColor('rgba(212, 132, 90, 0.12)')).toEqual({ r: 212, g: 132, b: 90, a: 0.12 });
  });

  test('parses rgb()', () => {
    expect(parseCssColor('rgb(18, 18, 24)')).toEqual({ r: 18, g: 18, b: 24, a: 1 });
  });

  test('returns null for oklch and junk', () => {
    expect(parseCssColor('oklch(0.16 0.01 280 / 0.85)')).toBeNull();
    expect(parseCssColor('')).toBeNull();
    expect(parseCssColor('var(--x)')).toBeNull();
  });
});

describe('contrastRatio', () => {
  const canvas = { r: 18, g: 18, b: 24, a: 1 }; // #121218

  test('black on white is 21:1', () => {
    expect(contrastRatio({ r: 0, g: 0, b: 0, a: 1 }, { r: 255, g: 255, b: 255, a: 1 })).toBeCloseTo(
      21,
      1
    );
  });

  test('same color is 1:1', () => {
    expect(contrastRatio(canvas, canvas)).toBeCloseTo(1, 5);
  });

  test('action blue #6b96d1 clears AA on the dark canvas', () => {
    const blue = parseCssColor('#6b96d1');
    if (!blue) throw new Error('parse failed');
    expect(contrastRatio(blue, canvas)).toBeGreaterThan(4.5);
  });

  test('primary foreground #0b0f14 clears AA on the action blue', () => {
    const fg = parseCssColor('#0b0f14');
    const blue = parseCssColor('#6b96d1');
    if (!fg || !blue) throw new Error('parse failed');
    expect(contrastRatio(fg, blue)).toBeGreaterThan(4.5);
  });

  test('translucent color composites over the backdrop first', () => {
    const halfWhite = { r: 255, g: 255, b: 255, a: 0.5 };
    const flat = blendOver(halfWhite, canvas);
    expect(contrastRatio(halfWhite, canvas)).toBeCloseTo(contrastRatio(flat, canvas), 5);
  });
});

test('formatRatio renders one decimal', () => {
  expect(formatRatio(6.137)).toBe('6.1:1');
  expect(formatRatio(21)).toBe('21.0:1');
});
