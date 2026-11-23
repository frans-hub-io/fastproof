import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { projectPath, request } from './client';
import { qk } from './hooks';
import type { components } from './schema.gen';

type S = components['schemas'];

export type MergeCounts = Required<S['MergeCounts']>;
export type LaunchMerge = Omit<S['LaunchMerge'], 'counts'> & { counts: MergeCounts };
export type LaunchMergeRequest = S['LaunchMergeRequest'];

export const MAX_MERGE_LAUNCHES = 20;

export const mergeKeys = {
  launch: (key: string, id: string) => ['merges', key, id] as const,
};

/** The merges a launch took part in, as target or source. */
export function useLaunchMerges(key: string, id: string, enabled = true) {
  return useQuery({
    queryKey: mergeKeys.launch(key, id),
    queryFn: () => request<{ items: LaunchMerge[] }>(`${projectPath(key)}/launches/${encodeURIComponent(id)}/merges`).then((r) => r.items),
    enabled: enabled && !!id,
  });
}

/** What a merge would do (`dryRun`), without changing anything. */
export function useMergePreview(key: string, body: LaunchMergeRequest | null) {
  return useQuery({
    queryKey: ['merge-preview', key, body] as const,
    queryFn: () => request<LaunchMerge>(`${projectPath(key)}/launches/merge`, { method: 'POST', json: { ...body, dryRun: true } }),
    enabled: !!body && body.launchIds.length >= 2,
    retry: false,
    staleTime: 5_000,
  });
}

function useInvalidateLaunches(key: string) {
  const qc = useQueryClient();
  return () => {
    void qc.invalidateQueries({ queryKey: qk.launches(key) });
    void qc.invalidateQueries({ queryKey: ['launch', key] });
    void qc.invalidateQueries({ queryKey: ['merges', key] });
    void qc.invalidateQueries({ queryKey: ['results', key] });
    void qc.invalidateQueries({ queryKey: qk.project(key) });
  };
}

export function useMergeLaunches(key: string) {
  const invalidate = useInvalidateLaunches(key);
  return useMutation({
    mutationFn: (body: LaunchMergeRequest) => request<LaunchMerge>(`${projectPath(key)}/launches/merge`, { method: 'POST', json: body }),
    onSuccess: invalidate,
  });
}

export function useUndoMerge(key: string) {
  const invalidate = useInvalidateLaunches(key);
  return useMutation({
    mutationFn: (id: string) => request<LaunchMerge>(`${projectPath(key)}/merges/${encodeURIComponent(id)}/undo`, { method: 'POST' }),
    onSuccess: invalidate,
  });
}
