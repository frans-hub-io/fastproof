/**
 * Short hints that tell apart items of one list that share a display name (same test name in
 * another suite, format or parameter set). Items with a unique name get no hint.
 */
export function sameNameHints<T>(items: T[], name: (t: T) => string, ids: (t: T) => string[]): Map<T, string> {
  const byName = new Map<string, T[]>();
  for (const t of items) {
    const n = name(t);
    const list = byName.get(n);
    if (list) list.push(t);
    else byName.set(n, [t]);
  }
  const out = new Map<T, string>();
  for (const group of byName.values()) {
    if (group.length < 2) continue;
    for (const t of group) {
      const own = ids(t);
      const pick = own.find((id, i) => group.every((o) => o === t || ids(o)[i] !== id)) ?? own[own.length - 1] ?? '';
      out.set(t, `#${shortId(pick)}`);
    }
  }
  return out;
}

function shortId(id: string): string {
  const tail = id.includes('.') ? id.slice(id.lastIndexOf('.') + 1) : id;
  return tail.replace(/-/g, '').slice(0, 8);
}

/** Names by result id and by the client id of every attempt (log lines name the attempt). */
export function resultNames(results: { id: string; name: string; clientIds?: string[] }[]): Map<string, string> {
  const m = new Map<string, string>();
  for (const r of results) {
    m.set(r.id, r.name);
    for (const c of r.clientIds ?? []) m.set(c, r.name);
  }
  return m;
}
