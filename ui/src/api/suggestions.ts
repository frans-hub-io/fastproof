// Triage suggestions, memory and backtest of FastProof Analysis. Suggestions are a
// transparent nearest-decision search over what people decided before (every "why" item is a
// row of a fixed weight table), not a trained model. The DTOs are the generated types.
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useAnalysisCapabilities } from './analysis';
import { projectPath, request, withQuery } from './client';
import type { components } from './schema.gen';

type S = components['schemas'];

export type LaunchSuggestions = S['LaunchSuggestions'];
export type ClusterSuggestions = S['ClusterSuggestions'];
export type Suggestion = S['Suggestion'];
export type SuggestionBand = S['SuggestionBand'];
export type WhyItem = S['WhyItem'];
export type SuggestionAccept = S['SuggestionAccept'];
export type SuggestionAccepted = S['SuggestionAccepted'];
export type MemoryList = S['MemoryList'];
export type BacktestReport = S['BacktestReport'];
export type BacktestBand = S['BacktestBand'];
export type AnalysisSettingsDoc = S['AnalysisSettings'];

export const suggestionKeys = {
  launch: (key: string, id: string) => ['suggestions', key, 'launch', id] as const,
  memory: (key: string, fp: string) => ['suggestions', key, 'memory', fp] as const,
  backtest: (key: string) => ['suggestions', key, 'backtest'] as const,
};

const enc = encodeURIComponent;

/** The suggestions of a launch by cluster. */
export function useLaunchSuggestions(key: string, launchId: string, enabled = true) {
  return useQuery({
    queryKey: suggestionKeys.launch(key, launchId),
    queryFn: () => request<LaunchSuggestions>(`${projectPath(key)}/launches/${enc(launchId)}/suggestions`),
    enabled: enabled && !!key && !!launchId,
    staleTime: 15_000,
  });
}

function useInvalidate(key: string) {
  const qc = useQueryClient();
  return () => {
    void qc.invalidateQueries({ queryKey: ['suggestions', key] });
    void qc.invalidateQueries({ queryKey: ['results', key] });
    void qc.invalidateQueries({ queryKey: ['result', key] });
    void qc.invalidateQueries({ queryKey: ['clusters', key] });
    void qc.invalidateQueries({ queryKey: ['launch', key] });
    void qc.invalidateQueries({ queryKey: ['analysis', key, 'history'] });
  };
}

/** Accepts a suggestion for its cluster (or listed results of it). */
export function useAcceptSuggestion(key: string) {
  const done = useInvalidate(key);
  return useMutation({
    mutationFn: ({ id, body }: { id: string; body?: SuggestionAccept }) =>
      request<SuggestionAccepted>(`${projectPath(key)}/suggestions/${enc(id)}/accept`, { method: 'POST', json: body ?? {} }),
    onSuccess: done,
  });
}

/** Rejects a suggestion (the memory is not changed). */
export function useRejectSuggestion(key: string) {
  const done = useInvalidate(key);
  return useMutation({
    mutationFn: ({ id, reason }: { id: string; reason?: string }) =>
      request<Suggestion>(`${projectPath(key)}/suggestions/${enc(id)}/reject`, { method: 'POST', json: reason ? { reason } : {} }),
    onSuccess: done,
  });
}

/** Makes a disabled rule (`match.fingerprint`) from a suggestion, for review. */
export function useSuggestionRule(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => request<{ id: string; name: string }>(`${projectPath(key)}/suggestions/${enc(id)}/rule`, { method: 'POST' }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ['suggestions', key] });
      void qc.invalidateQueries({ queryKey: ['rules', key] });
    },
  });
}

/** Computes the suggestions of a launch again (rate-limited by the server). */
export function useRecomputeSuggestions(key: string, launchId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: () => request<unknown>(`${projectPath(key)}/launches/${enc(launchId)}/suggestions/recompute`, { method: 'POST' }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ['analysis', key, 'launch', launchId] });
      void qc.invalidateQueries({ queryKey: suggestionKeys.launch(key, launchId) });
    },
  });
}

/** What people decided for a fingerprint. */
export function useMemory(key: string, fingerprint: string | null | undefined, enabled = true) {
  return useQuery({
    queryKey: suggestionKeys.memory(key, fingerprint ?? ''),
    queryFn: () => request<MemoryList>(withQuery(`${projectPath(key)}/analysis/memory`, { fingerprint })),
    enabled: enabled && !!fingerprint,
  });
}

/** The newest backtest report (`null` without one). */
export function useLatestBacktest(key: string, enabled = true) {
  return useQuery({
    queryKey: suggestionKeys.backtest(key),
    queryFn: () =>
      request<BacktestReport>(`${projectPath(key)}/analysis/backtest/latest`).catch((e: unknown) => {
        if ((e as { status?: number }).status === 404) return null;
        throw e;
      }),
    enabled,
  });
}

/** Queues a backtest (a task); the report shows when it is done. */
export function useRunBacktest(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (days?: number) =>
      request<{ id: string; status: string }>(`${projectPath(key)}/analysis/backtest`, { method: 'POST', json: days ? { days } : {} }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: suggestionKeys.backtest(key) });
    },
  });
}

/** The label of a band, with its backtest precision when the server sends one. */
export function bandLabel(s: Pick<Suggestion, 'band' | 'precision'>): string {
  const p = s.precision;
  return p === null || p === undefined ? s.band : `${s.band} ${Math.round(p * 100)}%`;
}

/** "Product bug · high · as in #431, PAY-1234". */
export function suggestionChip(s: Suggestion): string {
  const parts = [s.defectName || s.group, bandLabel(s)];
  const ref = [s.sample?.launchNumber ? `#${s.sample.launchNumber}` : null, s.issueKey ?? null].filter(Boolean).join(', ');
  if (ref) parts.push(`as in ${ref}`);
  return parts.join(' · ');
}

/** The suggestions of one cluster of a launch, from the launch's suggestions (one request per
 * launch); `on` when the server computes suggestions. */
export function useClusterSuggestions(project: string, launchId: string | null | undefined, fingerprint: string | null | undefined) {
  const caps = useAnalysisCapabilities(project);
  const on = !!caps.data?.features.suggestions;
  const q = useLaunchSuggestions(project, launchId ?? '', on && !!fingerprint);
  const cluster = q.data?.clusters.find((c) => c.fingerprint === fingerprint) ?? null;
  return { on, q, cluster };
}
