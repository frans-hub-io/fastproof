// FastProof Analysis core: capabilities, the analysis state of a launch, and the triage
// journal with its undo. The DTOs are the generated `schema.gen.ts` types.
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { projectPath, request, withQuery } from './client';
import type { components } from './schema.gen';

type S = components['schemas'];

export type AnalysisCapabilities = S['AnalysisCapabilities'];
export type AnalysisFeatures = S['AnalysisFeatures'];
export type AnalysisLaunchStatus = S['AnalysisLaunchStatus'];
export type LaunchSteps = S['LaunchSteps'];
export type StepOutcome = S['StepOutcome'];
export type TriageDecision = S['TriageDecision'];
export type TriageHistory = S['TriageHistory'];
export type TriageState = S['TriageState'];
export type TriageUndoResult = S['TriageUndoResult'];
export type DecisionVia = S['DecisionVia'];

export const analysisKeys = {
  capabilities: (key: string) => ['analysis', key, 'capabilities'] as const,
  launch: (key: string, id: string) => ['analysis', key, 'launch', id] as const,
  history: (key: string, q: { resultId?: string; batchId?: string }) => ['analysis', key, 'history', q] as const,
};

/** What Analysis answers for a project; cached for a minute. */
export function useAnalysisCapabilities(key: string | undefined) {
  return useQuery({
    queryKey: analysisKeys.capabilities(key ?? ''),
    queryFn: () => request<AnalysisCapabilities>(`${projectPath(key ?? '')}/analysis/capabilities`),
    enabled: !!key,
    staleTime: 60_000,
    retry: false,
  });
}

/** The analysis state of a launch (`none` when it was never queued). */
export function useLaunchAnalysis(key: string, id: string, enabled = true) {
  return useQuery({
    queryKey: analysisKeys.launch(key, id),
    queryFn: () => request<AnalysisLaunchStatus>(`${projectPath(key)}/launches/${encodeURIComponent(id)}/analysis`),
    enabled: enabled && !!id,
  });
}

/** The journal of a result (newest first) or of a batch. */
export function useTriageHistory(key: string, q: { resultId?: string; batchId?: string }, enabled = true) {
  return useQuery({
    queryKey: analysisKeys.history(key, q),
    queryFn: () => request<TriageHistory>(withQuery(`${projectPath(key)}/triage/history`, { ...q, limit: 100 })),
    enabled: enabled && !!(q.resultId ?? q.batchId),
  });
}

/** Undoes a batch; the results and their journal are fetched again. */
export function useUndoTriage(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (batchId: string) => request<TriageUndoResult>(`${projectPath(key)}/triage/undo`, { method: 'POST', json: { batchId } }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ['analysis', key, 'history'] });
      void qc.invalidateQueries({ queryKey: ['results', key] });
      void qc.invalidateQueries({ queryKey: ['result', key] });
      void qc.invalidateQueries({ queryKey: ['launch', key] });
    },
  });
}
