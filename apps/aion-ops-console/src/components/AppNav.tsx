import { cn } from '@/lib/utils';

export type AppNavItem = {
  /** Stable key + path used to compute the active state and as the link href. */
  href: string;
  /** Human label rendered in the nav. */
  label: string;
  /**
   * When set, the item is active if the current path starts with this prefix
   * (so `/workflows/:id` keeps "List" lit). Defaults to an exact match on href.
   */
  activePrefix?: string;
};

export type AppNavProps = {
  items: readonly AppNavItem[];
  /** Current router pathname; injected so the component stays pure. */
  currentPath: string;
  /** Link renderer injected by the caller (react-router <Link>) to avoid a hard dep. */
  renderLink: (item: AppNavItem, props: AppNavLinkProps) => React.ReactNode;
};

export type AppNavLinkProps = {
  className: string;
  'aria-current': 'page' | undefined;
};

function isActive(item: AppNavItem, currentPath: string): boolean {
  if (item.activePrefix !== undefined) {
    if (item.activePrefix === '/') {
      return currentPath === '/';
    }
    return currentPath === item.activePrefix || currentPath.startsWith(`${item.activePrefix}/`);
  }
  return currentPath === item.href;
}

/**
 * Presentational primary nav. Pure: route paths and the link renderer are
 * injected. Hand-plane: plain text links, active state via weight + color +
 * an underline marker (no chrome, no background pills).
 */
export function AppNav({ items, currentPath, renderLink }: AppNavProps) {
  return (
    <nav aria-label="Primary" className="flex items-center gap-5 text-sm">
      {items.map((item) => {
        const active = isActive(item, currentPath);
        const className = cn(
          'border-b-2 pb-1 transition-colors',
          active
            ? 'border-foreground font-medium text-foreground'
            : 'border-transparent text-muted-foreground hover:text-foreground'
        );

        return (
          <span key={item.href}>
            {renderLink(item, {
              className,
              'aria-current': active ? 'page' : undefined,
            })}
          </span>
        );
      })}
    </nav>
  );
}
