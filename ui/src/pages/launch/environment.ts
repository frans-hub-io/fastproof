import { useSearchParams } from 'react-router';
import { useLaunchEnvironments } from '../../api/hooks';
import { decodeValue, encodeValue, NONE_VALUE } from '../../lib/explorer';
import type { Counts, EnvironmentCounts, Launch } from '../../api/types';

/** The `env` value of results whose series has no environment key (`%00`). */
export const NONE_ENV = encodeValue(NONE_VALUE);

/** The `env` URL value of an environment: its key escaped like a list item, or the marker. */
export function envKey(e: EnvironmentCounts): string {
  return typeof e.key === 'string' ? encodeValue(e.key) : NONE_ENV;
}

export function envName(key: string): string {
  return key === NONE_ENV ? 'No environment' : decodeValue(key);
}

const ZERO: Counts = { total: 0, passed: 0, failed: 0, broken: 0, skipped: 0, unknown: 0, flaky: 0, muted: 0 };

export interface EnvironmentScope {
  /** The environment picked in the URL (`env`), if any. */
  env: string | undefined;
  list: EnvironmentCounts[];
  /** The counts of the picked environment; `undefined` for all of them. */
  counts: Counts | undefined;
  /** Whether the launch page shows the switcher. */
  show: boolean;
}

/** The environments of a launch and the one picked in the URL. */
export function useEnvironment(project: string, launch: Launch | undefined): EnvironmentScope {
  const [sp] = useSearchParams();
  const q = useLaunchEnvironments(project, launch?.id ?? '', launch?.status === 'running');
  const env = sp.get('env') || undefined;
  const list = q.data ?? [];
  const counts = env ? (list.find((e) => envKey(e) === env)?.counts ?? ZERO) : undefined;
  return { env, list, counts, show: list.length > 1 || !!env };
}
