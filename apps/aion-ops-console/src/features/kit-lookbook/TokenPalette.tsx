import { useEffect, useRef, useState } from 'react';
import { cn } from '@/lib/utils';
import { contrastRatio, formatRatio, parseCssColor, type Rgba } from './contrast';

// The palette tuning surface: every color token as a named swatch row —
// resolved value, chip on canvas + on card, the -glow companion, and for
// statuses the real 10px dot-on-canvas demo. Values come straight from the
// live stylesheet (getComputedStyle), so this page always tells the truth
// about what the console is actually rendering, in whichever theme is active.

type TokenSpec = {
  token: string;
  /** Companion 12%-alpha glow token, rendered alongside. */
  glow?: string;
  /** Usage note shown under the token name. */
  note?: string;
  /** Render the exact 10px status-dot demo (dot + 3px glow ring on canvas). */
  dot?: boolean;
  /** Chips render "Ag" in the token color instead of a filled block. */
  text?: boolean;
  /** Show contrast of --text-primary ON this token instead of vs canvas. */
  surface?: boolean;
};

type TokenGroup = { title: string; note?: string; tokens: TokenSpec[] };

const TOKEN_GROUPS: readonly TokenGroup[] = [
  {
    title: 'Accents',
    note: 'Blue acts; terracotta demands attention. Focus ring follows the action accent at 50%.',
    tokens: [
      { token: '--accent-primary', glow: '--accent-primary-glow', note: 'action · interactive' },
      { token: '--accent-primary-hover', note: 'action hover' },
      {
        token: '--accent-attention',
        glow: '--accent-attention-glow',
        note: 'attention · standout chip, brand moments',
      },
    ],
  },
  {
    title: 'Status set',
    note: 'One way to say it everywhere: 10px dot + 3px glow ring, dot demo on canvas.',
    tokens: [
      {
        token: '--status-success',
        glow: '--status-success-glow',
        note: 'healthy / complete',
        dot: true,
      },
      {
        token: '--status-warning',
        glow: '--status-warning-glow',
        note: 'running / working',
        dot: true,
      },
      {
        token: '--status-danger',
        glow: '--status-danger-glow',
        note: 'failed / destructive',
        dot: true,
      },
      {
        token: '--status-special',
        glow: '--status-special-glow',
        note: 'sub-agent / special',
        dot: true,
      },
      { token: '--status-live', glow: '--status-live-glow', note: 'live / streaming', dot: true },
    ],
  },
  {
    title: 'Canvas & surfaces',
    tokens: [
      { token: '--background', note: 'canvas', surface: true },
      { token: '--surface-base', surface: true },
      { token: '--surface-default', surface: true },
      { token: '--surface-elevated', surface: true },
      { token: '--surface-card', surface: true },
      { token: '--surface-hover', surface: true },
      { token: '--surface-code', surface: true },
    ],
  },
  {
    title: 'Text',
    tokens: [
      { token: '--text-primary', text: true },
      { token: '--text-secondary', text: true },
      { token: '--text-muted', text: true },
    ],
  },
  {
    title: 'Borders & focus',
    tokens: [
      { token: '--border-subtle' },
      { token: '--border-default' },
      { token: '--border-focus', note: 'action accent at 50%' },
    ],
  },
  {
    title: 'Glass',
    note: 'Summoned surfaces only — palettes, floating panels, islands.',
    tokens: [{ token: '--glass-bg' }, { token: '--glass-border' }],
  },
];

const ALL_TOKEN_NAMES = TOKEN_GROUPS.flatMap((group) =>
  group.tokens.flatMap((spec) => (spec.glow ? [spec.token, spec.glow] : [spec.token]))
).concat(['--text-primary']);

type ResolvedTokens = Record<string, string>;

/**
 * Resolve token values from the live cascade at a node inside the themed
 * tree, re-resolving when the html class flips theme.
 */
function useResolvedTokens(): {
  probeRef: React.RefObject<HTMLDivElement | null>;
  values: ResolvedTokens;
} {
  const probeRef = useRef<HTMLDivElement>(null);
  const [values, setValues] = useState<ResolvedTokens>({});

  useEffect(() => {
    const resolve = () => {
      const probe = probeRef.current;
      if (!probe) return;
      const style = getComputedStyle(probe);
      setValues(
        Object.fromEntries(
          ALL_TOKEN_NAMES.map((name) => [name, style.getPropertyValue(name).trim()])
        )
      );
    };
    resolve();
    const observer = new MutationObserver(resolve);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
    return () => observer.disconnect();
  }, []);

  return { probeRef, values };
}

function ratioLabel(spec: TokenSpec, values: ResolvedTokens): string | null {
  const canvas = parseCssColor(values['--background'] ?? '');
  const own = parseCssColor(values[spec.token] ?? '');
  if (!canvas || !own) return null;
  if (spec.surface) {
    const textPrimary = parseCssColor(values['--text-primary'] ?? '');
    if (!textPrimary) return null;
    const flat: Rgba = own.a < 1 ? { ...own, a: 1 } : own;
    return `text ${formatRatio(contrastRatio(textPrimary, flat))}`;
  }
  return `${formatRatio(contrastRatio(own, canvas))} on canvas`;
}

function Chip({ surfaceToken, spec }: { surfaceToken: string; spec: TokenSpec }) {
  return (
    <span
      className="inline-flex items-center justify-center rounded-md border border-[var(--border-subtle)] p-1.5"
      style={{ backgroundColor: `var(${surfaceToken})` }}
    >
      {spec.text ? (
        <span
          className="inline-flex size-7 items-center justify-center text-sm font-semibold"
          style={{ color: `var(${spec.token})` }}
        >
          Ag
        </span>
      ) : (
        <span className="size-7 rounded" style={{ backgroundColor: `var(${spec.token})` }} />
      )}
    </span>
  );
}

function GlowChip({ glow }: { glow: string }) {
  return (
    <span className="inline-flex items-center justify-center rounded-md border border-[var(--border-subtle)] bg-[var(--background)] p-1.5">
      <span className="size-7 rounded" style={{ backgroundColor: `var(${glow})` }} />
    </span>
  );
}

function DotDemo({ spec }: { spec: TokenSpec }) {
  return (
    <span className="inline-flex size-10 items-center justify-center rounded-md border border-[var(--border-subtle)] bg-[var(--background)]">
      <span
        className="rounded-full"
        style={{
          width: 10,
          height: 10,
          backgroundColor: `var(${spec.token})`,
          boxShadow: spec.glow ? `0 0 0 3px var(${spec.glow})` : undefined,
        }}
      />
    </span>
  );
}

function SwatchRow({ spec, values }: { spec: TokenSpec; values: ResolvedTokens }) {
  const resolved = values[spec.token] ?? '';
  const glowResolved = spec.glow ? (values[spec.glow] ?? '') : null;
  const ratio = ratioLabel(spec, values);

  return (
    <div
      className={cn(
        'grid items-center gap-x-4 gap-y-1 py-2',
        'grid-cols-[minmax(11rem,1.3fr)_minmax(9rem,1fr)_auto_auto_auto_auto]'
      )}
      data-token={spec.token}
    >
      <div className="flex min-w-0 flex-col">
        <span className="mono truncate text-[11px] text-[var(--text-primary)]">{spec.token}</span>
        {spec.note ? (
          <span className="text-[10px] text-[var(--text-muted)]">{spec.note}</span>
        ) : null}
      </div>
      <div className="flex min-w-0 flex-col">
        <span className="mono truncate text-[11px] text-[var(--text-secondary)]">
          {resolved || '—'}
        </span>
        {ratio ? <span className="mono text-[10px] text-[var(--text-muted)]">{ratio}</span> : null}
        {glowResolved ? (
          <span className="mono truncate text-[10px] text-[var(--text-muted)]">
            glow {glowResolved}
          </span>
        ) : null}
      </div>
      <Chip spec={spec} surfaceToken="--background" />
      <Chip spec={spec} surfaceToken="--surface-card" />
      {spec.glow ? <GlowChip glow={spec.glow} /> : <span className="size-10" aria-hidden="true" />}
      {spec.dot ? <DotDemo spec={spec} /> : <span className="size-10" aria-hidden="true" />}
    </div>
  );
}

const COLUMN_HEADERS = ['token', 'resolved', 'canvas', 'card', 'glow', 'dot'] as const;

export function TokenPalette() {
  const { probeRef, values } = useResolvedTokens();

  return (
    <div className="flex flex-col gap-5 overflow-x-auto" ref={probeRef}>
      <div className="min-w-[38rem]">
        <div className="grid grid-cols-[minmax(11rem,1.3fr)_minmax(9rem,1fr)_auto_auto_auto_auto] gap-x-4 border-b border-[var(--border-subtle)] pb-1.5">
          {COLUMN_HEADERS.map((header) => (
            <span
              className="text-[10px] font-semibold uppercase tracking-[0.15em] text-[var(--text-muted)]"
              key={header}
            >
              {header}
            </span>
          ))}
        </div>
        {TOKEN_GROUPS.map((group) => (
          <section className="flex flex-col pt-4" key={group.title}>
            <div className="flex flex-col gap-0.5 pb-1">
              <h3 className="text-[11px] font-semibold uppercase tracking-[0.15em] text-[var(--text-secondary)]">
                {group.title}
              </h3>
              {group.note ? (
                <p className="text-[11px] text-[var(--text-muted)]">{group.note}</p>
              ) : null}
            </div>
            <div className="divide-y divide-[var(--border-subtle)]">
              {group.tokens.map((spec) => (
                <SwatchRow key={spec.token} spec={spec} values={values} />
              ))}
            </div>
          </section>
        ))}
      </div>
    </div>
  );
}
