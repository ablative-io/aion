// WCAG 2.x contrast math for the /kit palette tuning surface. Parses only the
// authored token forms (hex + rgb/rgba) — tokens are decided in index.css, so
// this is not a general CSS color parser. oklch tokens resolve to null and the
// palette simply skips their ratio.

export type Rgba = { r: number; g: number; b: number; a: number };

function parseHex(hex: string): Rgba | null {
  if (hex.length === 3 || hex.length === 4) {
    const [r, g, b, a] = hex.split('').map((c) => Number.parseInt(c + c, 16));
    return { r: r ?? 0, g: g ?? 0, b: b ?? 0, a: a === undefined ? 1 : a / 255 };
  }
  if (hex.length === 6 || hex.length === 8) {
    return {
      r: Number.parseInt(hex.slice(0, 2), 16),
      g: Number.parseInt(hex.slice(2, 4), 16),
      b: Number.parseInt(hex.slice(4, 6), 16),
      a: hex.length === 8 ? Number.parseInt(hex.slice(6, 8), 16) / 255 : 1,
    };
  }
  return null;
}

function parseRgbFunction(args: string): Rgba | null {
  const parts = args
    .split(/[,\s/]+/)
    .filter(Boolean)
    .map(Number);
  if (parts.length < 3 || parts.some(Number.isNaN)) return null;
  const [r, g, b, a] = parts;
  return { r: r ?? 0, g: g ?? 0, b: b ?? 0, a: a ?? 1 };
}

export function parseCssColor(value: string): Rgba | null {
  const v = value.trim();
  const hex = /^#([0-9a-f]{3,8})$/i.exec(v)?.[1];
  if (hex) return parseHex(hex);
  const fn = /^rgba?\(([^)]+)\)$/i.exec(v)?.[1];
  if (fn) return parseRgbFunction(fn);
  return null;
}

/** Composite a (possibly translucent) color over an opaque backdrop. */
export function blendOver(fg: Rgba, backdrop: Rgba): Rgba {
  const a = fg.a;
  return {
    r: fg.r * a + backdrop.r * (1 - a),
    g: fg.g * a + backdrop.g * (1 - a),
    b: fg.b * a + backdrop.b * (1 - a),
    a: 1,
  };
}

function channelLuminance(channel: number): number {
  const c = channel / 255;
  return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
}

export function relativeLuminance({ r, g, b }: Rgba): number {
  return 0.2126 * channelLuminance(r) + 0.7152 * channelLuminance(g) + 0.0722 * channelLuminance(b);
}

/** WCAG contrast ratio (1–21); translucent colors composite over the backdrop first. */
export function contrastRatio(color: Rgba, backdrop: Rgba): number {
  const flat = color.a < 1 ? blendOver(color, backdrop) : color;
  const l1 = relativeLuminance(flat);
  const l2 = relativeLuminance(backdrop);
  const [darker, lighter] = l1 < l2 ? [l1, l2] : [l2, l1];
  return (lighter + 0.05) / (darker + 0.05);
}

export function formatRatio(ratio: number): string {
  return `${ratio.toFixed(1)}:1`;
}
