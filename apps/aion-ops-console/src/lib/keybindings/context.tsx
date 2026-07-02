import { createContext, type ReactNode, useContext, useEffect, useRef, useState } from 'react';

import { CONSOLE_ACTIONS } from './actions';
import { type ActionHandler, KeybindingRegistry } from './registry';

const KeybindingsContext = createContext<KeybindingRegistry | null>(null);

export type KeybindingsProviderProps = {
  children: ReactNode;
  /** Injected registry (tests); defaults to one built from the console inventory. */
  registry?: KeybindingRegistry | undefined;
};

/**
 * Owns THE single global key listener. Every keydown in the app funnels through
 * the registry's dispatch — no component attaches its own key listener, ever.
 * The listener mounts once per provider lifetime and never re-binds on
 * navigation (the registry identity is stable).
 */
export function KeybindingsProvider({ children, registry }: KeybindingsProviderProps) {
  const [instance] = useState(
    () => registry ?? new KeybindingRegistry({ actions: CONSOLE_ACTIONS })
  );

  useEffect(() => {
    const listener = (event: KeyboardEvent) => {
      instance.dispatch({
        key: event.key,
        metaKey: event.metaKey,
        ctrlKey: event.ctrlKey,
        altKey: event.altKey,
        shiftKey: event.shiftKey,
        target: event.target instanceof HTMLElement ? event.target : null,
        preventDefault: () => event.preventDefault(),
      });
    };

    window.addEventListener('keydown', listener);
    return () => window.removeEventListener('keydown', listener);
  }, [instance]);

  return <KeybindingsContext.Provider value={instance}>{children}</KeybindingsContext.Provider>;
}

/** The registry: format bindings for display, remap, or register handlers. */
export function useKeybindings(): KeybindingRegistry {
  const registry = useContext(KeybindingsContext);
  if (registry === null) {
    throw new Error('useKeybindings must be used within a KeybindingsProvider');
  }
  return registry;
}

/**
 * Register a handler for an action id while the calling component is mounted —
 * the ONLY sanctioned way for a component to react to a key. The handler ref is
 * kept fresh without re-registering, so registration order (and therefore
 * view-over-chrome precedence) is stable across renders.
 */
export function useAction(id: string, handler: ActionHandler): void {
  const registry = useKeybindings();
  const handlerRef = useRef(handler);
  handlerRef.current = handler;

  useEffect(
    () => registry.registerHandler(id, (event) => handlerRef.current(event)),
    [registry, id]
  );
}
