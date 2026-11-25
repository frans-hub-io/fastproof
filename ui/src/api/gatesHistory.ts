// Gate history and simulation of FastProof Analysis. The DTOs are the generated types.
import { useInfiniteQuery, useMutation } from '@tanstack/react-query';
import { projectPath, request, withQuery } from './client';
import type { components } from './schema.gen';

type S = components['schemas'];

export type GateHistory = S['GateHistory'];
export type GateHistoryItem = S['GateHistoryItem'];
export type GateHistoryGate = S['GateHistoryGate'];
export type GateHistoryBranch = S['GateHistoryBranch'];
export type GateSimulate = S['GateSimulate'];
export type GateSimulation = S['GateSimulation'];

export interface GateHistoryFilters {
  gate?: string;
  branch?: string;
  /** Days back from now (1 to 365). */
  days: number;
}

/** The history pages, newest first; the first page carries the aggregates. */
export function useGateHistory(key: string, f: GateHistoryFilters) {
  return useInfiniteQuery({
    queryKey: ['analysis', key, 'gate-history', f] as const,
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      request<GateHistory>(
        withQuery(`${projectPath(key)}/gates/history`, {
          gate: f.gate,
          branch: f.branch,
          from: new Date(Date.now() - f.days * 86_400_000).toISOString(),
          before: pageParam,
          limit: 50,
        }),
      ),
    getNextPageParam: (last) => last.next ?? undefined,
  });
}

/** Runs a draft gate over stored launches; nothing is written. */
export function useSimulateGate(key: string) {
  return useMutation({
    mutationFn: (body: GateSimulate) => request<GateSimulation>(`${projectPath(key)}/gates/simulate`, { method: 'POST', json: body }),
  });
}
