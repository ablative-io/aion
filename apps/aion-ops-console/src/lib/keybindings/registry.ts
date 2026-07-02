import {
  formatBinding,
  isEditableTarget,
  type KeyEventLike,
  type ParsedBinding,
  parseBinding,
  stepMatchesEvent,
} from './bindings';
import { detectPlatform, type KeyPlatform } from './platform';

/**
 * Where an action may fire from. `global` actions are live app-wide (their
 * handlers are registered by always-mounted chrome); `view` actions are live
 * only while a mounted view holds a handler registration — an unmounted view's
 * bindings are inert, and a view-scoped match takes precedence over a global
 * one on the same binding.
 */
export type KeyScope = 'global' | 'view';

export type ActionDefinition = {
  id: string;
  title: string;
  scope: KeyScope;
  /** Binding spec: `mod+k`, `g w`, `/`, `escape`. `mod` = ⌘/Super, never Ctrl. */
  defaultBinding: string;
  /**
   * Fire even when focus sits in an input/textarea/contenteditable. Default
   * false: plain-key bindings never steal keystrokes from a field.
   */
  allowInInputs?: boolean;
};

export type ActionHandler = (event: KeyEventLike) => void;

/** Minimal storage seam (localStorage-shaped) so persistence is testable. */
export type StorageLike = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;

export type KeybindingRegistryOptions = {
  actions: readonly ActionDefinition[];
  platform?: KeyPlatform;
  storage?: StorageLike | undefined;
  storageKey?: string;
  /** How long a chord prefix (`g`) stays armed waiting for its second key. */
  chordTimeoutMs?: number;
  now?: () => number;
};

export const KEYBINDINGS_STORAGE_KEY = 'aion:keybindings';
const DEFAULT_CHORD_TIMEOUT_MS = 1_200;

type RegisteredAction = {
  definition: ActionDefinition;
  binding: ParsedBinding;
  handlers: ActionHandler[];
};

/**
 * THE central keybinding registry — the only place in the app that interprets
 * keyboard events into actions. No component listens for keys ad-hoc; chrome
 * and views register HANDLERS against action ids (the seam), and the single
 * global listener (mounted by `KeybindingsProvider`) feeds events through
 * {@link dispatch}. Bindings are remappable per-user: overrides persist to
 * localStorage under {@link KEYBINDINGS_STORAGE_KEY} and survive reloads.
 */
export class KeybindingRegistry {
  readonly platform: KeyPlatform;

  private readonly actions = new Map<string, RegisteredAction>();
  private readonly storage: StorageLike | undefined;
  private readonly storageKey: string;
  private readonly chordTimeoutMs: number;
  private readonly now: () => number;
  private pendingChordAt: number | null = null;
  private pendingChordKey: string | null = null;

  constructor(options: KeybindingRegistryOptions) {
    this.platform = options.platform ?? detectPlatform();
    this.storage = options.storage ?? defaultStorage();
    this.storageKey = options.storageKey ?? KEYBINDINGS_STORAGE_KEY;
    this.chordTimeoutMs = options.chordTimeoutMs ?? DEFAULT_CHORD_TIMEOUT_MS;
    this.now = options.now ?? Date.now;

    const overrides = this.loadOverrides();
    for (const definition of options.actions) {
      const spec = overrides[definition.id] ?? definition.defaultBinding;
      this.actions.set(definition.id, {
        definition,
        binding: safeParse(spec) ?? parseBinding(definition.defaultBinding),
        handlers: [],
      });
    }
  }

  listActions(): ActionDefinition[] {
    return [...this.actions.values()].map((entry) => entry.definition);
  }

  /** The effective binding spec for an action (override or default). */
  bindingFor(id: string): string {
    const overrides = this.loadOverrides();
    return overrides[id] ?? this.mustGet(id).definition.defaultBinding;
  }

  /** The effective binding formatted for this platform (⌘K / Sup+K). */
  formatFor(id: string): string {
    return formatBinding(this.bindingFor(id), this.platform);
  }

  /** Remap an action. Persists and takes effect immediately. */
  setOverride(id: string, spec: string): void {
    const entry = this.mustGet(id);
    entry.binding = parseBinding(spec);
    const overrides = this.loadOverrides();
    overrides[id] = spec;
    this.saveOverrides(overrides);
  }

  /** Restore an action's default binding. */
  clearOverride(id: string): void {
    const entry = this.mustGet(id);
    entry.binding = parseBinding(entry.definition.defaultBinding);
    const overrides = this.loadOverrides();
    delete overrides[id];
    this.saveOverrides(overrides);
  }

  /**
   * Register a handler for an action — the registration seam. Returns the
   * unregister function. The most recent registration wins when several exist
   * (a mounted view layers over chrome); an action with no handlers is inert.
   */
  registerHandler(id: string, handler: ActionHandler): () => void {
    const entry = this.mustGet(id);
    entry.handlers.push(handler);

    return () => {
      const index = entry.handlers.indexOf(handler);
      if (index !== -1) {
        entry.handlers.splice(index, 1);
      }
    };
  }

  /**
   * Dispatch one keyboard event through the registry. Returns whether an
   * action consumed it (in which case `preventDefault` was called). Respects
   * scope (view-scoped matches beat global ones), input focus (only opt-in
   * bindings fire from editable targets), and two-step chords (`g w`).
   */
  dispatch(event: KeyEventLike): boolean {
    if (isModifierOnly(event.key)) {
      return false;
    }

    const candidates = this.liveCandidates(event);

    if (this.chordArmed()) {
      const resolved = this.resolveChord(event, candidates);
      this.clearChord();
      if (resolved) {
        return true;
      }
      // A failed second key falls through: it may itself start a new match.
    }

    const single = pickByScope(
      candidates.filter((entry) => entry.binding.length === 1 && matchesStep(entry, 0, event))
    );
    if (single !== undefined) {
      return this.fire(single, event);
    }

    const prefix = candidates.some(
      (entry) => entry.binding.length === 2 && matchesStep(entry, 0, event)
    );
    if (prefix) {
      this.pendingChordAt = this.now();
      this.pendingChordKey = event.key.toLowerCase();
      event.preventDefault?.();
      return true;
    }

    return false;
  }

  private resolveChord(event: KeyEventLike, candidates: RegisteredAction[]): boolean {
    const matched = pickByScope(
      candidates.filter(
        (entry) =>
          entry.binding.length === 2 &&
          entry.binding[0]?.key === this.pendingChordKey &&
          matchesStep(entry, 1, event)
      )
    );

    return matched === undefined ? false : this.fire(matched, event);
  }

  private chordArmed(): boolean {
    return (
      this.pendingChordAt !== null &&
      this.pendingChordKey !== null &&
      this.now() - this.pendingChordAt <= this.chordTimeoutMs
    );
  }

  private clearChord(): void {
    this.pendingChordAt = null;
    this.pendingChordKey = null;
  }

  /** Actions that could fire for this event: handled + not input-suppressed. */
  private liveCandidates(event: KeyEventLike): RegisteredAction[] {
    const editable = isEditableTarget(event.target);

    return [...this.actions.values()].filter(
      (entry) => entry.handlers.length > 0 && (!editable || entry.definition.allowInInputs === true)
    );
  }

  private fire(entry: RegisteredAction, event: KeyEventLike): boolean {
    const handler = entry.handlers.at(-1);
    if (handler === undefined) {
      return false;
    }
    event.preventDefault?.();
    handler(event);
    return true;
  }

  private mustGet(id: string): RegisteredAction {
    const entry = this.actions.get(id);
    if (entry === undefined) {
      throw new Error(`unknown keybinding action "${id}"`);
    }
    return entry;
  }

  private loadOverrides(): Record<string, string> {
    const raw = this.storage?.getItem(this.storageKey);
    if (raw === null || raw === undefined) {
      return {};
    }
    try {
      const parsed: unknown = JSON.parse(raw);
      if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
        return {};
      }
      const overrides: Record<string, string> = {};
      for (const [id, spec] of Object.entries(parsed)) {
        if (typeof spec === 'string') {
          overrides[id] = spec;
        }
      }
      return overrides;
    } catch {
      // A corrupt persisted blob falls back to defaults, never crashes input.
      return {};
    }
  }

  private saveOverrides(overrides: Record<string, string>): void {
    this.storage?.setItem(this.storageKey, JSON.stringify(overrides));
  }
}

function matchesStep(entry: RegisteredAction, index: number, event: KeyEventLike): boolean {
  const step = entry.binding[index];
  return step !== undefined && stepMatchesEvent(step, event);
}

/** View-scoped matches take precedence over global ones on the same binding. */
function pickByScope(matches: RegisteredAction[]): RegisteredAction | undefined {
  return matches.find((entry) => entry.definition.scope === 'view') ?? matches[0];
}

function isModifierOnly(key: string): boolean {
  return key === 'Meta' || key === 'Control' || key === 'Alt' || key === 'Shift';
}

function safeParse(spec: string): ParsedBinding | null {
  try {
    return parseBinding(spec);
  } catch {
    return null;
  }
}

function defaultStorage(): StorageLike | undefined {
  return typeof localStorage === 'undefined' ? undefined : localStorage;
}
