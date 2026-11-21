import { flatItems, useLaunches } from '../api/hooks';
import type { Launch } from '../api/types';
import { formatRelative } from './format';

export function launchOptionLabel(l: Launch): string {
  const branch = l.vcs?.branch && !l.name.includes(l.vcs.branch) ? ` · ${l.vcs.branch}` : '';
  return `#${String(l.number)} ${l.name}${branch} · ${formatRelative(l.startedAt)}`;
}

export function useLaunchList(project: string): { launches: Launch[]; pending: boolean } {
  const q = useLaunches(project, {});
  return { launches: flatItems(q.data), pending: q.isPending };
}
