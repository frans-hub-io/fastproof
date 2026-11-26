// Flaky causes (FastProof Analysis): the likely causes of one flaky series with their evidence.
// The DTOs are the generated `schema.gen.ts` types; `evidence` is a free object read here.
import { useQuery } from '@tanstack/react-query';
import { projectPath, request } from './client';
import type { components } from './schema.gen';

type S = components['schemas'];

export type FlakyHintKind = S['FlakyHintKind'];
export type HintConfidence = S['HintConfidence'];
export type HintAction = S['HintAction'];
export type FlakyHintBrief = S['FlakyHintBrief'];
export type SignalSource = S['SignalSource'];
export type FlakyHint = Omit<S['FlakyHint'], 'evidence'> & { evidence: HintEvidence };
export type FlakyHints = Omit<S['FlakyHints'], 'hints'> & { hints: FlakyHint[] };

/** A run that supports a hint. */
export interface HintExample {
  launchId: string;
  resultId?: string;
  at: string;
  status: 'failed' | 'passedOnRetry' | 'passed' | 'other';
}

/** The evidence of a hint: counts and names by kind, plus example runs. */
export type HintEvidence = Record<string, unknown> & { examples?: HintExample[] };

export const HINT_KIND_LABEL: Record<FlakyHintKind, string> = {
  timing: 'Timing',
  environment: 'Environment',
  order: 'Test order',
  retry: 'Passes on retry',
  infra: 'Infrastructure',
  time: 'Time of day',
};

export const HINT_KIND_HELP: Record<FlakyHintKind, string> = {
  timing: 'Failures are timeouts or stop at a fixed duration that passing runs come close to.',
  environment: 'One host, CI runner or environment fails far more often than the others (Fisher’s exact test).',
  order: 'Fails together with another test, or only after a certain test ran earlier on the same worker.',
  retry: 'Fails with changing errors and passes on a retry of the same run.',
  infra: 'Failures coincide with grid, CI, Kubernetes or mock signals, or with a burst of infrastructure errors.',
  time: 'Fails more often at some hours or weekdays (UTC); needs the long history of ClickHouse.',
};

export const CONFIDENCE_COLOR: Record<HintConfidence, string> = {
  high: 'red',
  medium: 'orange',
  low: 'gray',
};

export const flakyHintKeys = {
  series: (key: string, seriesId: string) => ['flaky-hints', key, seriesId] as const,
};

/** The causes of one series; `null` id: nothing is fetched. */
export function useFlakyHints(key: string, seriesId: string | null) {
  return useQuery({
    queryKey: flakyHintKeys.series(key, seriesId ?? ''),
    queryFn: () => request<FlakyHints>(`${projectPath(key)}/flaky/${encodeURIComponent(seriesId ?? '')}/hints`),
    enabled: !!key && !!seriesId,
  });
}

/** Example runs of a hint (newest first), tolerant of a missing or unexpected field. */
export function examplesOf(h: FlakyHint): HintExample[] {
  const e = h.evidence.examples;
  return Array.isArray(e) ? e.filter((x): x is HintExample => typeof x === 'object' && typeof x.launchId === 'string') : [];
}

/** The newest result among the hints' examples (the target of a mute or an issue). */
export function latestResult(hints: FlakyHint[]): HintExample | null {
  const all = hints.flatMap(examplesOf).filter((x) => !!x.resultId);
  all.sort((a, b) => b.at.localeCompare(a.at));
  return all[0] ?? null;
}
