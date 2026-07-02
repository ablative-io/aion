import type { KeyPlatform } from './platform';

/**
 * One step of a binding: a key plus its exact modifier set. `mod` is the
 * PRIMARY modifier — `metaKey` (⌘ on macOS, Super on Linux). Control is never
 * a primary modifier in this app (the modifier rule), so a parsed step has no
 * ctrl slot at all and a ctrl-modified event can never match any binding.
 */
export type BindingStep = {
  key: string;
  mod: boolean;
  alt: boolean;
  shift: boolean;
};

/** A binding: a single step (`mod+k`) or a two-step sequence (`g w`). */
export type ParsedBinding = BindingStep[];

/** The subset of `KeyboardEvent` a binding matches against (test-constructible). */
export type KeyEventLike = {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  target?: EventTargetLike | null;
  preventDefault?: () => void;
};

/** Structural view of an event target, enough to decide input suppression. */
export type EventTargetLike = {
  tagName?: string;
  isContentEditable?: boolean;
};

const MODIFIER_TOKENS = new Set(['mod', 'alt', 'shift']);
const FORBIDDEN_TOKENS = new Set(['ctrl', 'control', 'ctl']);

/**
 * Parse a binding spec: steps separated by spaces (`g w`), modifiers within a
 * step separated by `+` (`mod+k`). Rejects any control-as-primary spec at
 * definition time — the modifier rule is enforced here, not by convention.
 */
export function parseBinding(spec: string): ParsedBinding {
  const steps = spec.trim().split(/\s+/);
  if (steps.length === 0 || steps.length > 2) {
    throw new Error(`binding "${spec}" must have one or two steps`);
  }

  return steps.map((step) => parseStep(step, spec));
}

function parseStep(step: string, spec: string): BindingStep {
  const tokens = step.toLowerCase().split('+');
  const key = tokens.at(-1);
  if (key === undefined || key.length === 0 || MODIFIER_TOKENS.has(key)) {
    throw new Error(`binding "${spec}" has no key in step "${step}"`);
  }

  const parsed: BindingStep = { key, mod: false, alt: false, shift: false };
  for (const token of tokens.slice(0, -1)) {
    if (FORBIDDEN_TOKENS.has(token)) {
      throw new Error(
        `binding "${spec}" uses Control as a modifier; the primary modifier is mod (⌘/Super)`
      );
    }
    if (!MODIFIER_TOKENS.has(token)) {
      throw new Error(`binding "${spec}" has unknown modifier "${token}"`);
    }
    parsed[token as 'mod' | 'alt' | 'shift'] = true;
  }

  return parsed;
}

/**
 * Whether an event matches one binding step. Exact-modifier matching, and a
 * ctrl-modified event never matches (Control is never primary — `mod` is
 * `metaKey` on every platform).
 */
export function stepMatchesEvent(step: BindingStep, event: KeyEventLike): boolean {
  if (event.ctrlKey) {
    return false;
  }

  return (
    event.key.toLowerCase() === step.key &&
    event.metaKey === step.mod &&
    event.altKey === step.alt &&
    // Shift is load-bearing only when the binding asks for it; a shifted
    // character key (e.g. `?`) already encodes shift in `event.key`.
    (step.shift ? event.shiftKey : step.key.length > 1 ? !event.shiftKey : true)
  );
}

/** Whether the event's focus target should suppress non-opt-in bindings. */
export function isEditableTarget(target: EventTargetLike | null | undefined): boolean {
  if (target === null || target === undefined) {
    return false;
  }
  if (target.isContentEditable === true) {
    return true;
  }

  const tag = target.tagName?.toUpperCase();
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT';
}

const MAC_MODIFIERS = { mod: '⌘', alt: '⌥', shift: '⇧' } as const;
const LINUX_MODIFIERS = { mod: 'Sup+', alt: 'Alt+', shift: 'Shift+' } as const;

const KEY_LABELS: Record<string, string> = {
  escape: 'Esc',
  tab: 'Tab',
  enter: '↵',
  arrowup: '↑',
  arrowdown: '↓',
  arrowleft: '←',
  arrowright: '→',
};

/** Format a binding for display: `mod+k` → `⌘K` (mac) / `Sup+K` (linux). */
export function formatBinding(spec: string, platform: KeyPlatform): string {
  const mods = platform === 'mac' ? MAC_MODIFIERS : LINUX_MODIFIERS;

  return parseBinding(spec)
    .map((step) => {
      const key =
        KEY_LABELS[step.key] ?? (step.key.length === 1 ? step.key.toUpperCase() : step.key);
      return `${step.mod ? mods.mod : ''}${step.alt ? mods.alt : ''}${step.shift ? mods.shift : ''}${key}`;
    })
    .join(' ');
}
