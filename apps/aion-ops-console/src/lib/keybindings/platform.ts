/** The two platforms the console formats bindings for. */
export type KeyPlatform = 'mac' | 'linux';

type NavigatorLike = {
  platform?: string;
  userAgent?: string;
};

/**
 * Detect the display platform for key formatting. On both platforms the primary
 * modifier is the SAME physical event bit (`metaKey`: ⌘ on macOS, Super on
 * Linux) — detection only affects how bindings are *displayed* (⌘K vs Sup+K),
 * never how they match.
 */
export function detectPlatform(nav?: NavigatorLike): KeyPlatform {
  const source = nav ?? (typeof navigator === 'undefined' ? undefined : navigator);
  const probe = `${source?.platform ?? ''} ${source?.userAgent ?? ''}`;

  return /mac|iphone|ipad|ipod/i.test(probe) ? 'mac' : 'linux';
}
