import { ChevronsUpDown, Maximize2, Minimize2 } from 'lucide-react';
import { AnimatePresence, motion, useReducedMotion } from 'motion/react';
import type { ReactNode } from 'react';
import { useEffect, useState } from 'react';

import { cn } from '@/lib/utils';

import { degradeToFade, MICRO_TRANSITION_SLOW, SPRING_SIGNATURE } from './springs';
import type { KitStatus } from './status-dot';
import { StatusDot } from './status-dot';

// The seed of the morphing-entity workspace (MORPHING-ENTITY-WORKSPACE.md):
// one continuous surface that exists at three form factors. Each smaller form
// is a *different form that is more useful at that size*, not a shrunken
// window — pill streams headlines, card adds context + a quick input, window
// hands the whole surface to its children.

export type EntityForm = 'pill' | 'card' | 'window';

export const ENTITY_FORMS: readonly EntityForm[] = ['pill', 'card', 'window'];

/** The next-larger form factor (window is the ceiling). */
export function expandedForm(form: EntityForm): EntityForm {
  if (form === 'pill') return 'card';
  return 'window';
}

/** The next-smaller form factor (pill is the floor). */
export function collapsedForm(form: EntityForm): EntityForm {
  if (form === 'window') return 'card';
  return 'pill';
}

export type EntityKeyboardActions = {
  expand: () => void;
  collapse: () => void;
};

/**
 * Pure factory for the entity's keyboard verbs (expand on Enter, collapse on
 * Escape — bound by the central keybinding registry, never by the entity
 * itself). No-ops at the form-factor ceiling/floor.
 */
export function createEntityKeyboardActions(
  form: EntityForm,
  onFormChange: (form: EntityForm) => void
): EntityKeyboardActions {
  return {
    expand: () => {
      const next = expandedForm(form);
      if (next !== form) onFormChange(next);
    },
    collapse: () => {
      const next = collapsedForm(form);
      if (next !== form) onFormChange(next);
    },
  };
}

export type EntityProps = {
  form: EntityForm;
  onFormChange?: (form: EntityForm) => void;
  /** The entity's identity — always visible, whatever the form. */
  name: string;
  status?: KitStatus;
  /** Live one-liners (log headlines, tool descriptions) rolled through the pill. */
  headlines?: readonly string[];
  /** Extra header content for the card/window forms (metrics, chips). */
  header?: ReactNode;
  /** Card-form quick-interaction slot — a one-line nudge without opening anything. */
  quickInput?: ReactNode;
  children?: ReactNode;
  className?: string;
  /**
   * Callback wiring surface for the central keybinding registry (track C):
   * receives the entity's expand/collapse verbs, may return a cleanup. The
   * entity never listens for keys itself.
   */
  // biome-ignore lint/suspicious/noConfusingVoidType: mirrors React's EffectCallback — registrars without cleanup return nothing
  registerKeyboardActions?: (actions: EntityKeyboardActions) => void | (() => void);
};

const ENTITY_FORM_CLASS: Record<EntityForm, string> = {
  pill: 'h-9 max-w-xs cursor-pointer rounded-full border border-[var(--border-default)] bg-[var(--surface-elevated,#16161d)] px-3 hover:bg-[var(--surface-hover)]',
  card: 'w-88 rounded-xl border border-[var(--border-default)] bg-[var(--surface-card,#1a1a22)] shadow-lg',
  window:
    'flex h-full w-full flex-col rounded-xl border border-[var(--border-default)] bg-[var(--surface-card,#1a1a22)] shadow-xl',
};

export function Entity({
  form,
  onFormChange,
  name,
  status = 'idle',
  headlines,
  header,
  quickInput,
  children,
  className,
  registerKeyboardActions,
}: EntityProps) {
  const reducedMotion = useReducedMotion() ?? false;
  const transition = degradeToFade(SPRING_SIGNATURE, reducedMotion);
  // Overflow choreography: content is clipped while the surface morphs, then
  // released once it settles so popovers/shadows can escape the surface.
  const [morphing, setMorphing] = useState(false);
  // Hover pauses the pill's headline stream; the button is the hover target.
  const [pillHovered, setPillHovered] = useState(false);

  useEffect(() => {
    if (!registerKeyboardActions || !onFormChange) return;
    const cleanup = registerKeyboardActions(createEntityKeyboardActions(form, onFormChange));
    return typeof cleanup === 'function' ? cleanup : undefined;
  }, [registerKeyboardActions, onFormChange, form]);

  const setForm = (next: EntityForm) => {
    if (next !== form) onFormChange?.(next);
  };

  return (
    <motion.div
      layout={!reducedMotion}
      transition={transition}
      onLayoutAnimationStart={() => setMorphing(true)}
      onLayoutAnimationComplete={() => setMorphing(false)}
      className={cn('flex flex-col', ENTITY_FORM_CLASS[form], className)}
      style={{ overflow: morphing ? 'hidden' : 'visible' }}
      data-slot="entity"
      data-form={form}
      data-morphing={morphing || undefined}
    >
      <AnimatePresence initial={false} mode="popLayout">
        {form === 'pill' ? (
          <EntityFormShell key="pill" reducedMotion={reducedMotion}>
            <button
              type="button"
              className="group/entity-pill flex h-full w-full min-w-0 items-center gap-2 text-left outline-none focus-visible:ring-2 focus-visible:ring-[var(--border-focus)]"
              onClick={() => setForm('card')}
              onMouseEnter={() => setPillHovered(true)}
              onMouseLeave={() => setPillHovered(false)}
              aria-label={`Expand ${name}`}
            >
              <StatusDot status={status} pulse={status === 'live' || status === 'running'} />
              <span className="shrink-0 text-xs font-medium text-[var(--text-primary)]">
                {name}
              </span>
              {headlines && headlines.length > 0 ? (
                <EntityPillStream
                  headlines={headlines}
                  paused={pillHovered}
                  reducedMotion={reducedMotion}
                />
              ) : null}
            </button>
          </EntityFormShell>
        ) : (
          <EntityFormShell key="expanded" reducedMotion={reducedMotion}>
            <EntityExpandedSurface
              form={form}
              name={name}
              status={status}
              header={header}
              quickInput={quickInput}
              setForm={setForm}
            >
              {children}
            </EntityExpandedSurface>
          </EntityFormShell>
        )}
      </AnimatePresence>
    </motion.div>
  );
}

function EntityExpandedSurface({
  form,
  name,
  status,
  header,
  quickInput,
  setForm,
  children,
}: {
  form: 'card' | 'window';
  name: string;
  status: KitStatus;
  header?: ReactNode;
  quickInput?: ReactNode;
  setForm: (form: EntityForm) => void;
  children?: ReactNode;
}) {
  return (
    <>
      <div className="flex items-center gap-2 border-b border-[var(--border-subtle,rgba(255,255,255,0.04))] px-4 py-3">
        <StatusDot status={status} pulse={status === 'live' || status === 'running'} />
        <span className="min-w-0 truncate text-sm font-semibold text-[var(--text-primary)]">
          {name}
        </span>
        {header ? <div className="min-w-0 flex-1">{header}</div> : <div className="flex-1" />}
        <EntityFormButton
          label={form === 'card' ? `Expand ${name} to window` : `Collapse ${name} to card`}
          onClick={() => setForm(form === 'card' ? 'window' : 'card')}
        >
          {form === 'card' ? (
            <Maximize2 aria-hidden="true" className="size-3.5" />
          ) : (
            <Minimize2 aria-hidden="true" className="size-3.5" />
          )}
        </EntityFormButton>
        <EntityFormButton label={`Collapse ${name} to pill`} onClick={() => setForm('pill')}>
          <ChevronsUpDown aria-hidden="true" className="size-3.5" />
        </EntityFormButton>
      </div>
      {form === 'card' ? (
        <div className="flex flex-col gap-3 px-4 py-3" data-slot="entity-card-body">
          {children}
          {quickInput ? <div data-slot="entity-quick-input">{quickInput}</div> : null}
        </div>
      ) : (
        <div className="min-h-0 flex-1 overflow-auto px-4 py-3" data-slot="entity-window-body">
          {children}
        </div>
      )}
    </>
  );
}

function EntityFormShell({
  children,
  reducedMotion,
}: {
  children: ReactNode;
  reducedMotion: boolean;
}) {
  return (
    <motion.div
      layout={!reducedMotion}
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0, filter: 'blur(10px)' }}
      transition={MICRO_TRANSITION_SLOW}
      className="flex min-h-0 flex-1 flex-col"
    >
      {children}
    </motion.div>
  );
}

function EntityFormButton({
  label,
  onClick,
  children,
}: {
  label: string;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      onClick={onClick}
      className="rounded-md p-1 text-[var(--text-muted)] transition-colors duration-150 hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)] focus-visible:ring-2 focus-visible:ring-[var(--border-focus)] outline-none"
    >
      {children}
    </button>
  );
}

const PILL_STREAM_INTERVAL_MS = 3200;

/**
 * The ambient life of the pill: headlines gently roll through it. Hovering
 * the pill pauses the roll (via the interactive parent's `paused` prop) and
 * group-hover lifts the text to full legibility; look away and it returns to
 * ambient.
 */
export function EntityPillStream({
  headlines,
  paused = false,
  reducedMotion,
}: {
  headlines: readonly string[];
  /** Pause is driven by the interactive parent (the pill is the hover target). */
  paused?: boolean;
  reducedMotion?: boolean;
}) {
  const [index, setIndex] = useState(0);

  useEffect(() => {
    if (paused || headlines.length < 2) return;
    const timer = setInterval(() => {
      setIndex((current) => (current + 1) % headlines.length);
    }, PILL_STREAM_INTERVAL_MS);
    return () => clearInterval(timer);
  }, [paused, headlines.length]);

  const headline = headlines[index % headlines.length] ?? '';

  return (
    <span
      className="relative min-w-0 flex-1 overflow-hidden text-xs text-[var(--text-secondary)] opacity-60 transition-opacity duration-150 group-hover/entity-pill:opacity-100"
      data-slot="entity-pill-stream"
    >
      <AnimatePresence initial={false} mode="wait">
        <motion.span
          key={`${index}-${headline}`}
          className="block truncate"
          initial={reducedMotion ? { opacity: 0 } : { opacity: 0, y: 10 }}
          animate={{ opacity: 1, y: 0 }}
          exit={reducedMotion ? { opacity: 0 } : { opacity: 0, y: -10 }}
          transition={MICRO_TRANSITION_SLOW}
        >
          {headline}
        </motion.span>
      </AnimatePresence>
    </span>
  );
}
