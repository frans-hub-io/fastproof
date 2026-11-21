const KEY = 'fastproof-recent-launches';
const MAX = 10;

export interface RecentLaunch {
  project: string;
  id: string;
  number: number;
  name: string;
}

export function readRecent(): RecentLaunch[] {
  try {
    const raw = window.localStorage.getItem(KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (x): x is RecentLaunch =>
        !!x && typeof x === 'object' && typeof (x as RecentLaunch).project === 'string' && typeof (x as RecentLaunch).id === 'string' && typeof (x as RecentLaunch).number === 'number',
    );
  } catch {
    return [];
  }
}

export function rememberLaunch(item: RecentLaunch): RecentLaunch[] {
  const next = [item, ...readRecent().filter((r) => !(r.project === item.project && r.id === item.id))].slice(0, MAX);
  try {
    window.localStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    return next;
  }
  window.dispatchEvent(new Event('fastproof-recent'));
  return next;
}
