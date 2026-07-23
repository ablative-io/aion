export function toggleSetMember(current: ReadonlySet<string>, id: string): ReadonlySet<string> {
  const next = new Set(current);
  if (next.has(id)) {
    next.delete(id);
  } else {
    next.add(id);
  }
  return next;
}

export function withoutPathAndDescendants(
  current: ReadonlySet<string>,
  path: string
): ReadonlySet<string> {
  return new Set(
    [...current].filter((candidate) => candidate !== path && !candidate.startsWith(`${path}>`))
  );
}
