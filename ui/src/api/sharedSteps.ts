// Shared steps (reusable steps with versions): the generated DTOs under the contract's names and the hooks of the
// `shared-steps` routes. A case references a shared step by id; runs pin its runnable version.
import { keepPreviousData, useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { projectPath, request, requestRaw, withQuery } from './client';
import { etagOf, getVersioned, tmk, writeVersioned, type Versioned } from './tm';
import type { Case, RunItem, TmJob } from './tm';
import type { components } from './schema.gen';

type S = components['schemas'];

export type SharedStep = S['SharedStep'];
export type SharedStepSummary = S['SharedStepSummary'];
export type SharedStepList = S['SharedStepList'];
export type SharedStepWrite = S['SharedStepWrite'];
export type SharedStepBrief = S['SharedStepBrief'];
export type SharedStepRef = S['SharedStepRef'];
export type SharedStepVersionSummary = S['SharedStepVersionSummary'];
export type SharedStepSnapshot = S['SharedStepSnapshot'];
export type SharedStepVersion = S['SharedStepVersion'];
export type SharedStepUsage = S['SharedStepUsage'];
export type SharedStepUsageList = S['SharedStepUsageList'];
export type SharedStepChange = S['SharedStepChange'];
export type SharedStepChangeList = S['SharedStepChangeList'];
export type ExtractSharedStep = S['ExtractSharedStep'];
export type ExtractedSharedStep = S['ExtractedSharedStep'];
export type SharedPin = S['SharedPin'];
export type SharedStepInlined = S['SharedStepInlined'];

const enc = encodeURIComponent;

export const ssk = {
  all: (key: string) => ['shared-steps', key] as const,
  list: (key: string, q: object) => ['shared-steps', key, 'list', q] as const,
  one: (key: string, n: number) => ['shared-steps', key, 'one', n] as const,
  versions: (key: string, n: number) => ['shared-steps', key, 'one', n, 'versions'] as const,
  version: (key: string, n: number, v: number) => ['shared-steps', key, 'one', n, 'version', v] as const,
  usages: (key: string, n: number) => ['shared-steps', key, 'one', n, 'usages'] as const,
  changes: (key: string, caseNumber: number) => ['shared-steps', key, 'case-changes', caseNumber] as const,
};

export const SHARED_PAGE = 50;

/** The key a person types and sees: `SS-12`. */
export function sharedKey(number: number): string {
  return `SS-${String(number)}`;
}

/** Whether a step is a reference to a shared step. */
export function isSharedRef(step: { shared?: SharedStepRef | null }): step is { shared: SharedStepRef } {
  return !!step.shared;
}

export interface SharedStepQuery {
  q?: string;
  archived?: boolean;
}

export function useSharedSteps(key: string, q: SharedStepQuery, enabled = true) {
  return useInfiniteQuery({
    queryKey: ssk.list(key, q),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      request<SharedStepList>(withQuery(`${projectPath(key)}/shared-steps`, { q: q.q, archived: q.archived ? 'true' : undefined, before: pageParam, limit: SHARED_PAGE })),
    getNextPageParam: (last) => last.next ?? undefined,
    placeholderData: keepPreviousData,
    enabled,
  });
}

export function useSharedStep(key: string, n: number) {
  return useQuery({
    queryKey: ssk.one(key, n),
    queryFn: () => getVersioned<SharedStep>(`${projectPath(key)}/shared-steps/${String(n)}`),
    enabled: Number.isInteger(n) && n > 0,
    staleTime: 0,
  });
}

export function useCreateSharedStep(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: SharedStepWrite) => request<SharedStep>(`${projectPath(key)}/shared-steps`, { method: 'POST', json: body }),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ssk.all(key) }),
  });
}

export function useUpdateSharedStep(key: string, n: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ body, etag }: { body: SharedStepWrite; etag: string }) => writeVersioned<SharedStep>(`${projectPath(key)}/shared-steps/${String(n)}`, 'PUT', etag, body),
    onSuccess: (v: Versioned<SharedStep>) => {
      qc.setQueryData(ssk.one(key, n), v);
      void qc.invalidateQueries({ queryKey: ssk.versions(key, n) });
      void qc.invalidateQueries({ queryKey: ['shared-steps', key, 'list'] });
    },
  });
}

export function useArchiveSharedStep(key: string, n: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (purge: boolean) => requestRaw(withQuery(`${projectPath(key)}/shared-steps/${String(n)}`, { purge: purge ? 'true' : undefined }), { method: 'DELETE' }),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ssk.all(key) }),
  });
}

export function useRestoreSharedStep(key: string, n: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: () => request<SharedStep>(`${projectPath(key)}/shared-steps/${String(n)}/restore`, { method: 'POST' }),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ssk.all(key) }),
  });
}

export function useSharedStepVersions(key: string, n: number, enabled = true) {
  return useQuery({
    queryKey: ssk.versions(key, n),
    queryFn: () => request<{ items: SharedStepVersionSummary[] }>(withQuery(`${projectPath(key)}/shared-steps/${String(n)}/versions`, { limit: 500 })).then((r) => r.items),
    enabled,
  });
}

export function useSharedStepVersion(key: string, n: number, v: number | null) {
  return useQuery({
    queryKey: ssk.version(key, n, v ?? 0),
    queryFn: () => request<SharedStepVersion>(`${projectPath(key)}/shared-steps/${String(n)}/versions/${String(v ?? 0)}`),
    enabled: v !== null && v > 0,
    staleTime: Infinity,
  });
}

export function useRestoreSharedStepVersion(key: string, n: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ version, etag }: { version: number; etag: string }) => writeVersioned<SharedStep>(`${projectPath(key)}/shared-steps/${String(n)}/versions/${String(version)}/restore`, 'POST', etag),
    onSuccess: (v: Versioned<SharedStep>) => {
      qc.setQueryData(ssk.one(key, n), v);
      void qc.invalidateQueries({ queryKey: ssk.versions(key, n) });
    },
  });
}

export function useSharedStepUsages(key: string, n: number, enabled = true) {
  return useInfiniteQuery({
    queryKey: ssk.usages(key, n),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => request<SharedStepUsageList>(withQuery(`${projectPath(key)}/shared-steps/${String(n)}/usages`, { before: pageParam, limit: 100 })),
    getNextPageParam: (last) => last.next ?? undefined,
    enabled,
  });
}

/** `200 {updated, skipped}` when done in the request, or the `shared_inline` job above 1000 usages. */
export type InlineResult = { kind: 'done'; result: SharedStepInlined } | { kind: 'job'; job: TmJob };

export function useInlineSharedStep(key: string, n: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (): Promise<InlineResult> => {
      const r = await requestRaw(`${projectPath(key)}/shared-steps/${String(n)}/inline`, { method: 'POST' });
      return r.status === 202 ? { kind: 'job', job: r.data as TmJob } : { kind: 'done', result: r.data as SharedStepInlined };
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ssk.all(key) });
      void qc.invalidateQueries({ queryKey: ['tm', key] });
    },
  });
}

/** Moves adjacent top-level steps of a case into a new shared step (needs the case's `ETag`). */
export function useExtractSharedStep(key: string, caseNumber: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async ({ body, etag }: { body: ExtractSharedStep; etag: string }) => {
      const r = await requestRaw(`${projectPath(key)}/cases/${String(caseNumber)}/extract-shared`, { method: 'POST', json: body, headers: { 'If-Match': etag } });
      return r.data as ExtractedSharedStep;
    },
    onSuccess: (r) => {
      qc.setQueryData(tmk.case(key, caseNumber), { value: r.case, etag: etagOf(r.case.revision) } satisfies Versioned<Case>);
      void qc.invalidateQueries({ queryKey: ssk.all(key) });
      void qc.invalidateQueries({ queryKey: tmk.versions(key, caseNumber) });
      void qc.invalidateQueries({ queryKey: ['tm', key, 'cases'] });
    },
  });
}

export function useCaseSharedStepChanges(key: string, caseNumber: number, enabled = true) {
  return useQuery({
    queryKey: ssk.changes(key, caseNumber),
    queryFn: () => request<SharedStepChangeList>(withQuery(`${projectPath(key)}/cases/${String(caseNumber)}/shared-step-changes`, { limit: 200 })).then((r) => r.items),
    enabled,
  });
}

/** Pins an item that has not started to the current versions of its shared steps. */
export function useRefreshRunItem(key: string, itemId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: () => getVersionedPost<RunItem>(`${projectPath(key)}/run-items/${enc(itemId)}/refresh`),
    onSuccess: (v: Versioned<RunItem>) => {
      qc.setQueryData(tmk.runItem(key, itemId), v);
      void qc.invalidateQueries({ queryKey: ssk.all(key) });
    },
  });
}

async function getVersionedPost<T extends { revision: number }>(path: string): Promise<Versioned<T>> {
  const r = await requestRaw(path, { method: 'POST' });
  const value = r.data as T;
  return { value, etag: r.headers.get('ETag') ?? `"r${String(value.revision)}"` };
}
