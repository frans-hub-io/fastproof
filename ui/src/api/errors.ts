// Unique errors (FastProof Analysis): the errors of a project with their state across branches,
// one error with its history, and the patch that ignores, activates, merges or splits errors.
// The DTOs are the generated `schema.gen.ts` types.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { projectPath, request, withQuery } from './client';
import type { components } from './schema.gen';

type S = components['schemas'];

export type ErrorState = S['ErrorState'];
export type ErrorSort = S['ErrorSort'];
export type ErrorSummary = S['ErrorSummary'];
export type ErrorList = S['ErrorList'];
export type ErrorDetail = S['ErrorDetail'];
export type ErrorBranch = S['ErrorBranch'];
export type ErrorTimeline = S['ErrorTimeline'];
export type ErrorTimelinePoint = S['ErrorTimelinePoint'];
export type ErrorPatch = S['ErrorPatch'];
export type MemoryEntry = S['MemoryEntry'];

export const ERROR_STATES: ErrorState[] = ['new', 'active', 'regressed', 'resolved', 'ignored'];

export const ERROR_STATE_LABEL: Record<ErrorState, string> = {
  new: 'New',
  active: 'Active',
  resolved: 'Resolved',
  regressed: 'Regressed',
  ignored: 'Ignored',
};

export const ERROR_SORT_LABEL: Record<ErrorSort, string> = {
  lastSeen: 'Last seen',
  firstSeen: 'First seen',
  occurrences: 'Occurrences',
  launches: 'Launches',
};

export interface ErrorFilter {
  state?: ErrorState;
  branch?: string;
  q?: string;
  sort?: ErrorSort;
  before?: string;
  limit?: number;
}

export const errorKeys = {
  all: (key: string) => ['errors', key] as const,
  list: (key: string, f: ErrorFilter) => ['errors', key, 'list', f] as const,
  detail: (key: string, fp: string) => ['errors', key, 'detail', fp] as const,
};

/** The errors of a project, one page. */
export function useErrors(key: string, f: ErrorFilter) {
  return useQuery({
    queryKey: errorKeys.list(key, f),
    queryFn: () => request<ErrorList>(withQuery(`${projectPath(key)}/errors`, { ...f })),
    enabled: !!key,
    placeholderData: keepPreviousData,
  });
}

/** One error with its branches, commits, decisions, tests and timeline. */
export function useError(key: string, fingerprint: string) {
  return useQuery({
    queryKey: errorKeys.detail(key, fingerprint),
    queryFn: () => request<ErrorDetail>(`${projectPath(key)}/errors/${encodeURIComponent(fingerprint)}`),
    enabled: !!key && !!fingerprint,
  });
}

/** Ignores, activates, merges (`aliasOf`) or splits (`aliasOf: null`) an error. */
export function usePatchError(key: string, fingerprint: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (patch: ErrorPatch) => request<ErrorDetail>(`${projectPath(key)}/errors/${encodeURIComponent(fingerprint)}`, { method: 'PATCH', json: patch }),
    onSuccess: (d) => {
      qc.setQueryData(errorKeys.detail(key, fingerprint), d);
      void qc.invalidateQueries({ queryKey: errorKeys.all(key) });
      void qc.invalidateQueries({ queryKey: ['clusters', key] });
    },
  });
}

/** Whether a string is a fingerprint (16 hex digits). */
export function isFingerprint(s: string): boolean {
  return /^[0-9a-f]{16}$/.test(s);
}

/** Seconds as "3 d 4 h", "5 h 10 min" or "12 min". */
export function formatSpan(seconds: number | null | undefined): string {
  if (seconds == null || seconds < 0) return '—';
  const m = Math.floor(seconds / 60);
  const h = Math.floor(m / 60);
  const d = Math.floor(h / 24);
  if (d > 0) return `${String(d)} d ${String(h % 24)} h`;
  if (h > 0) return `${String(h)} h ${String(m % 60)} min`;
  return `${String(Math.max(m, seconds > 0 ? 1 : 0))} min`;
}
