import { Package, Rocket, TerminalSquare } from 'lucide-react';
import { motion } from 'motion/react';
import type { ReactNode } from 'react';
import { useEffect, useState } from 'react';
import type { ChatPriority, EntityForm, EscalationLevel, KitStatus } from '@/components/kit';
import {
  AnimatedBackground,
  ChatInputMorph,
  Disclosure,
  DisclosureContent,
  DisclosureTrigger,
  Entity,
  ExpandableRow,
  FloatingPanelBody,
  FloatingPanelContent,
  FloatingPanelFooter,
  FloatingPanelRoot,
  FloatingPanelTrigger,
  KIT_ACCENT,
  MorphingPopover,
  MorphingPopoverContent,
  MorphingPopoverTrigger,
  SlidingNumber,
  SPRING_SECONDARY,
  SPRING_SIGNATURE,
  SPRING_SUCCESS,
  StatusDot,
  TransitionPanel,
  useReducedMotionTransition,
} from '@/components/kit';
import { cn } from '@/lib/utils';
import { TokenPalette } from './TokenPalette';

// The design lookbook: every kit component in every state, with knobs. This
// is the human verification surface for Phase 0 — not linked from the nav,
// summoned at /kit when eyes-on verification is needed.

export function KitLookbook() {
  return (
    <div className="mx-auto flex w-full max-w-4xl flex-col gap-10 px-6 py-8">
      <header className="flex flex-col gap-1">
        <h1 className="text-lg font-semibold tracking-[-0.02em] text-[var(--text-primary)]">
          The motion kit
        </h1>
        <p className="text-sm text-[var(--text-secondary)]">
          Phase 0 material — one physics, one status vocabulary, morphing surfaces. Toggle your OS
          reduced-motion setting to verify every spring degrades to a fade.
        </p>
      </header>

      <PaletteSection />
      <SpringsSection />
      <StatusSection />
      <EntitySection />
      <ChatSection />
      <PopoverSection />
      <ExpandableSection />
      <AnimatedBackgroundSection />
      <DisclosureSection />
      <TransitionPanelSection />
      <SlidingNumberSection />
    </div>
  );
}

function Section({ title, note, children }: { title: string; note?: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-3">
      <div className="flex flex-col gap-0.5">
        <h2 className="text-[11px] font-semibold uppercase tracking-[0.15em] text-[var(--text-muted)]">
          {title}
        </h2>
        {note ? <p className="text-xs text-[var(--text-secondary)]">{note}</p> : null}
      </div>
      <div className="rounded-xl border border-[var(--border-default)] bg-[var(--surface-base)] p-4">
        {children}
      </div>
    </section>
  );
}

function Knob({
  active = false,
  children,
  onClick,
}: {
  active?: boolean;
  children: ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'rounded-lg border px-2.5 py-1 text-xs transition-colors duration-150',
        'outline-none focus-visible:ring-2 focus-visible:ring-[var(--border-focus)]',
        active
          ? 'border-transparent font-semibold text-[var(--surface-base,#0f0f14)]'
          : 'border-[var(--border-default)] text-[var(--text-secondary)] hover:bg-[var(--surface-hover)]'
      )}
      style={active ? { backgroundColor: KIT_ACCENT } : undefined}
    >
      {children}
    </button>
  );
}

function PaletteSection() {
  return (
    <Section
      title="Palette"
      note="Every color token, resolved live from the active theme — the tuning surface. Ratios are WCAG contrast; statuses show the real 10px dot on canvas."
    >
      <TokenPalette />
    </Section>
  );
}

function SpringsSection() {
  const [nudged, setNudged] = useState(false);
  const signature = useReducedMotionTransition(SPRING_SIGNATURE);
  const secondary = useReducedMotionTransition(SPRING_SECONDARY);
  const success = useReducedMotionTransition(SPRING_SUCCESS);
  const springs = [
    { label: 'signature 550/45/0.7', transition: signature },
    { label: 'secondary 350/35', transition: secondary },
    { label: 'success 500/22', transition: success },
  ];

  return (
    <Section
      title="House physics"
      note="The three springs, side by side. Signature for surface morphs, secondary for elements, success with a touch of bounce."
    >
      <div className="flex flex-col gap-3">
        {springs.map((spring) => (
          <div key={spring.label} className="flex items-center gap-4">
            <span className="mono w-44 shrink-0 text-[10px] text-[var(--text-muted)]">
              {spring.label}
            </span>
            <div className="relative h-6 flex-1">
              <motion.div
                animate={{ x: nudged ? 'min(28rem, 100%)' : 0, opacity: 1 }}
                transition={spring.transition}
                className="absolute size-6 rounded-md"
                style={{ backgroundColor: KIT_ACCENT }}
              />
            </div>
          </div>
        ))}
        <div>
          <Knob onClick={() => setNudged((current) => !current)}>Nudge</Knob>
        </div>
      </div>
    </Section>
  );
}

const ALL_STATUSES: readonly KitStatus[] = [
  'healthy',
  'running',
  'failed',
  'special',
  'live',
  'idle',
];

function StatusSection() {
  return (
    <Section
      title="Status vocabulary"
      note="One way to say it everywhere: dot + glow. Live pulses; blue acts; terracotta demands attention; no cyan."
    >
      <div className="flex flex-wrap items-center gap-6">
        {ALL_STATUSES.map((status) => (
          <span
            key={status}
            className="flex items-center gap-2 text-xs text-[var(--text-secondary)]"
          >
            <StatusDot pulse={status === 'live'} status={status} />
            {status}
          </span>
        ))}
      </div>
    </Section>
  );
}

const DEMO_HEADLINES = [
  'Bash: cargo test --workspace',
  'Read: crates/aion-server/src/lib.rs',
  'Edit: fixed quorum denominator',
  'thinking about shard placement…',
] as const;

function EntitySection() {
  const [form, setForm] = useState<EntityForm>('pill');

  return (
    <Section
      title="Entity"
      note="The morphing-entity seed: pill ⇄ card ⇄ window, one continuous surface. The pill streams headlines (hover to pause); Enter/Escape wire in through the keybinding registry."
    >
      <div className="flex flex-col gap-4">
        <div className="flex gap-2">
          {(['pill', 'card', 'window'] satisfies EntityForm[]).map((option) => (
            <Knob active={form === option} key={option} onClick={() => setForm(option)}>
              {option}
            </Knob>
          ))}
        </div>
        <div className={cn('flex', form === 'window' ? 'h-72' : 'min-h-72 items-start')}>
          <Entity
            form={form}
            headlines={[...DEMO_HEADLINES]}
            header={<span className="text-xs text-[var(--text-muted)]">step 3/7 · dev loop</span>}
            name="wf-faaa1b04"
            onFormChange={setForm}
            quickInput={
              <input
                className="w-full rounded-lg border border-[var(--border-default)] bg-transparent px-2.5 py-1.5 text-xs text-[var(--text-primary)] outline-none placeholder:text-[var(--text-muted)] focus-visible:ring-2 focus-visible:ring-[var(--border-focus)]"
                placeholder="Quick nudge…"
              />
            }
            status="running"
          >
            <div className="flex flex-col gap-2 text-xs text-[var(--text-secondary)]">
              <p>Recent events roll up here in card form; the window form owns the surface.</p>
              <ul className="flex list-disc flex-col gap-1 pl-4">
                {DEMO_HEADLINES.map((headline) => (
                  <li key={headline}>{headline}</li>
                ))}
              </ul>
            </div>
          </Entity>
        </div>
      </div>
    </Section>
  );
}

function ChatSection() {
  const [streaming, setStreaming] = useState(true);
  const [log, setLog] = useState<string[]>([]);
  const append = (entry: string) => setLog((current) => [...current.slice(-4), entry]);

  return (
    <Section
      title="Chat-input morph"
      note="Deliberately light: pill → textarea and nothing more. ⌘↵ sends, Esc collapses, drafts survive collapse. While streaming, send = interrupt and re-presses escalate → shutdown → kill (3s decay)."
    >
      <div className="flex flex-col gap-3">
        <div className="flex items-center gap-2">
          <Knob active={streaming} onClick={() => setStreaming((current) => !current)}>
            streaming
          </Knob>
          <span className="text-xs text-[var(--text-muted)]">
            {streaming ? 'run is live — button escalates' : 'idle — button just sends'}
          </span>
        </div>
        <ChatInputMorph
          capabilities={['interrupt', 'queue']}
          draftKey="lookbook"
          onEscalate={(action: EscalationLevel) => append(`escalate: ${action}`)}
          onSubmit={(message: string, priority: ChatPriority) =>
            append(`sent (${priority}): ${message}`)
          }
          status={streaming ? 'live' : 'idle'}
          streaming={streaming}
        />
        {log.length > 0 ? (
          <ol className="mono flex flex-col gap-0.5 text-[10px] text-[var(--text-muted)]">
            {log.map((entry, index) => (
              // biome-ignore lint/suspicious/noArrayIndexKey: append-only demo log
              <li key={index}>{entry}</li>
            ))}
          </ol>
        ) : null}
      </div>
    </Section>
  );
}

function PopoverSection() {
  return (
    <Section
      title="MorphingPopover + FloatingPanel"
      note="Trigger→surface shared-layout morphs, anchored, non-blocking. Escape or click-outside dismisses; focus returns to the trigger."
    >
      <div className="flex min-h-48 flex-wrap items-start gap-4">
        <MorphingPopover>
          <MorphingPopoverTrigger>
            <Package aria-hidden="true" className="size-3.5" />
            Inspect payload
          </MorphingPopoverTrigger>
          <MorphingPopoverContent className="w-72">
            <pre className="mono overflow-x-auto text-[11px] text-[var(--text-secondary)]">
              {JSON.stringify({ shard: 3, owner: 'node-b', epoch: 41 }, null, 2)}
            </pre>
          </MorphingPopoverContent>
        </MorphingPopover>

        <FloatingPanelRoot title="Deploy package">
          <FloatingPanelTrigger>
            <Rocket aria-hidden="true" className="size-3.5" />
            Deploy…
          </FloatingPanelTrigger>
          <FloatingPanelContent>
            <FloatingPanelBody>
              <p className="text-xs text-[var(--text-secondary)]">
                A structured mini-surface: header, body, footer. Small workflows live here instead
                of a modal.
              </p>
            </FloatingPanelBody>
            <FloatingPanelFooter>
              <span className="text-[10px] uppercase tracking-[0.15em] text-[var(--text-muted)]">
                esc closes
              </span>
            </FloatingPanelFooter>
          </FloatingPanelContent>
        </FloatingPanelRoot>
      </div>
    </Section>
  );
}

function ExpandableSection() {
  const [controlledOpen, setControlledOpen] = useState(false);

  return (
    <Section
      title="Expandable row"
      note="One-line summary spring-expanding to measured-height detail. First row uncontrolled, second controlled by the knob."
    >
      <div className="flex flex-col gap-2">
        <ExpandableRow
          icon={<TerminalSquare aria-hidden="true" className="size-3.5" />}
          pips={3}
          pipsFilled={2}
          status="running"
          summary="Bash: cargo test --workspace"
        >
          <pre className="mono overflow-x-auto text-[11px]">
            running 128 tests{'\n'}test quorum::adopts_shard ... ok{'\n'}test placement::rebalances
            ... ok
          </pre>
        </ExpandableRow>
        <div className="flex items-center gap-2">
          <Knob active={controlledOpen} onClick={() => setControlledOpen((current) => !current)}>
            controlled: {controlledOpen ? 'expanded' : 'collapsed'}
          </Knob>
        </div>
        <ExpandableRow
          expanded={controlledOpen}
          onExpandedChange={setControlledOpen}
          status="healthy"
          summary="Edit: crates/aion-server/src/quorum.rs"
        >
          <p className="text-xs">Controlled detail — the knob above and this row share state.</p>
        </ExpandableRow>
      </div>
    </Section>
  );
}

const TABS = ['Workflows', 'Incidents', 'Registry'] as const;

function AnimatedBackgroundSection() {
  return (
    <Section
      title="AnimatedBackground"
      note="The shared-layout highlight slides between items instead of blinking. Click row selects; hover row follows the pointer."
    >
      <div className="flex flex-col gap-3">
        <div className="flex gap-1">
          <AnimatedBackground defaultValue={TABS[0]}>
            {TABS.map((tab) => (
              <button
                className="px-3 py-1.5 text-xs text-[var(--text-secondary)] data-[checked=true]:text-[var(--text-primary)]"
                data-id={tab}
                key={tab}
                type="button"
              >
                {tab}
              </button>
            ))}
          </AnimatedBackground>
        </div>
        <div className="flex gap-1">
          <AnimatedBackground enableHover>
            {TABS.map((tab) => (
              <button
                className="px-3 py-1.5 text-xs text-[var(--text-secondary)]"
                data-id={`hover-${tab}`}
                key={tab}
                type="button"
              >
                {tab}
              </button>
            ))}
          </AnimatedBackground>
        </div>
      </div>
    </Section>
  );
}

function DisclosureSection() {
  return (
    <Section title="Disclosure" note="Animated collapsible for detail panels.">
      <Disclosure className="max-w-md">
        <DisclosureTrigger>Show attempt metadata</DisclosureTrigger>
        <DisclosureContent>
          <dl className="mono grid grid-cols-2 gap-1 text-[11px]">
            <dt className="text-[var(--text-muted)]">attempt</dt>
            <dd>2</dd>
            <dt className="text-[var(--text-muted)]">worker</dt>
            <dd>node-b/worker-1</dd>
            <dt className="text-[var(--text-muted)]">queue</dt>
            <dd>agents</dd>
          </dl>
        </DisclosureContent>
      </Disclosure>
    </Section>
  );
}

function TransitionPanelSection() {
  const [activeIndex, setActiveIndex] = useState(0);

  return (
    <Section
      title="TransitionPanel"
      note="Directional view switch — panels arrive from the direction of travel."
    >
      <div className="flex flex-col gap-3">
        <div className="flex gap-2">
          {TABS.map((tab, index) => (
            <Knob active={activeIndex === index} key={tab} onClick={() => setActiveIndex(index)}>
              {tab}
            </Knob>
          ))}
        </div>
        <TransitionPanel activeIndex={activeIndex} className="min-h-16">
          {TABS.map((tab) => (
            <div className="text-sm text-[var(--text-secondary)]" key={tab}>
              The {tab.toLowerCase()} view content, sliding in from the travel direction.
            </div>
          ))}
        </TransitionPanel>
      </div>
    </Section>
  );
}

function SlidingNumberSection() {
  const [seconds, setSeconds] = useState(0);
  const [count, setCount] = useState(128);

  useEffect(() => {
    const timer = setInterval(() => setSeconds((current) => current + 1), 1000);
    return () => clearInterval(timer);
  }, []);

  return (
    <Section
      title="SlidingNumber"
      note="Per-digit odometer. Bling guard: only where the number IS the information (live durations, live counts) — everything else just renders."
    >
      <div className="flex flex-wrap items-center gap-8">
        <div className="flex items-baseline gap-2 text-xl text-[var(--text-primary)]">
          <SlidingNumber padStart={2} value={Math.floor(seconds / 60)} />
          <span className="text-[var(--text-muted)]">:</span>
          <SlidingNumber padStart={2} value={seconds % 60} />
          <span className="text-xs text-[var(--text-muted)]">live duration</span>
        </div>
        <div className="flex items-center gap-3">
          <Knob onClick={() => setCount((current) => Math.max(0, current - 7))}>-7</Knob>
          <span className="text-xl text-[var(--text-primary)]">
            <SlidingNumber value={count} />
          </span>
          <Knob onClick={() => setCount((current) => current + 7)}>+7</Knob>
        </div>
      </div>
    </Section>
  );
}
