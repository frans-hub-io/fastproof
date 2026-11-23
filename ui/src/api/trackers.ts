// Issues in trackers (Jira, GitLab, GitHub, Azure DevOps) created or linked from FastProof and
// the state read back from them.
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useMemo } from 'react';
import { projectPath, request, withQuery } from './client';
import type { components } from './schema.gen';

type S = components['schemas'];

export type TrackerIssueCategory = 'creating' | 'open' | 'closed';
export type TrackerIssue = Omit<S['TrackerIssue'], 'category'> & {
  category: TrackerIssueCategory;
};
export type TrackerIssueDetail = Omit<S['TrackerIssueDetail'], 'category'> & {
  category: TrackerIssueCategory;
};
export type TrackerIssueLink = S['TrackerIssueLink'];
export type CreateTrackerIssue = S['CreateTrackerIssue'];
export type TrackerIssueCreated = Omit<S['TrackerIssueCreated'], 'issue'> & {
  issue?: TrackerIssue | null;
};
export type LinkTrackerIssue = S['LinkTrackerIssue'];
export type UnmuteOutcome = S['UnmuteOutcome'];
export type TrackerTest = S['TrackerTest'];
export type TrackerApi = S['TrackerApi'];

export interface TrackerIssueFilter {
  state?: TrackerIssueCategory;
  unmuteSuggested?: boolean;
  tracker?: string;
  launchId?: string;
  resultId?: string;
}

const enc = encodeURIComponent;

export const qkt = {
  all: (key: string) => ['trackerIssues', key] as const,
  list: (key: string, f: TrackerIssueFilter) => ['trackerIssues', key, 'list', f] as const,
  lookup: (key: string, urls: string[]) => ['trackerIssues', key, 'lookup', urls] as const,
  one: (key: string, id: string) => ['trackerIssues', key, 'one', id] as const,
};

export function useTrackerIssues(key: string, f: TrackerIssueFilter) {
  return useQuery({
    queryKey: qkt.list(key, f),
    queryFn: () => request<{ items: TrackerIssue[]; next?: string | null }>(withQuery(`${projectPath(key)}/tracker-issues`, { ...f, limit: 200 })),
    placeholderData: keepPreviousData,
  });
}

/** The tracked issues behind these issue URLs (defects on a page of results), by URL. */
export function useIssueStates(key: string, urls: (string | null | undefined)[]): Map<string, TrackerIssue> {
  const wanted = useMemo(() => [...new Set(urls.filter((u): u is string => !!u && /^https?:\/\//.test(u)))].sort().slice(0, 100), [urls]);
  const q = useQuery({
    queryKey: qkt.lookup(key, wanted),
    queryFn: () => request<{ items: TrackerIssue[] }>(`${projectPath(key)}/tracker-issues/lookup`, { method: 'POST', json: { urls: wanted } }),
    enabled: wanted.length > 0,
    staleTime: 30_000,
  });
  return useMemo(() => new Map((q.data?.items ?? []).filter((i) => i.url).map((i) => [i.url as string, i] as const)), [q.data]);
}

export function useTrackerIssue(key: string, id: string | null) {
  return useQuery({
    queryKey: qkt.one(key, id ?? ''),
    queryFn: () => request<TrackerIssueDetail>(`${projectPath(key)}/tracker-issues/${enc(id ?? '')}`),
    enabled: !!id,
  });
}

function useInvalidate(key: string) {
  const qc = useQueryClient();
  return () => {
    void qc.invalidateQueries({ queryKey: qkt.all(key) });
    void qc.invalidateQueries({ queryKey: ['results', key] });
    void qc.invalidateQueries({ queryKey: ['result', key] });
    void qc.invalidateQueries({ queryKey: ['clusters', key] });
    void qc.invalidateQueries({ queryKey: ['launch', key] });
  };
}

export function useCreateTrackerIssue(key: string) {
  const done = useInvalidate(key);
  return useMutation({
    mutationFn: (body: CreateTrackerIssue) =>
      request<TrackerIssueCreated>(`${projectPath(key)}/tracker-issues`, {
        method: 'POST',
        json: body,
      }),
    onSuccess: (r) => {
      if (r.issue) done();
    },
  });
}

/** The rendered title and description, nothing is sent. */
export function useIssuePreview(key: string, body: CreateTrackerIssue | null) {
  return useQuery({
    queryKey: ['trackerIssues', key, 'preview', body],
    queryFn: () =>
      request<TrackerIssueCreated>(`${projectPath(key)}/tracker-issues`, {
        method: 'POST',
        json: { ...body, dryRun: true },
      }),
    enabled: !!body,
    staleTime: Infinity,
    retry: false,
  });
}

export function useLinkTrackerIssue(key: string) {
  const done = useInvalidate(key);
  return useMutation({
    mutationFn: (body: LinkTrackerIssue) =>
      request<TrackerIssueCreated>(`${projectPath(key)}/tracker-issues/link`, {
        method: 'POST',
        json: body,
      }),
    onSuccess: done,
  });
}

export function useSyncTrackerIssue(key: string) {
  const done = useInvalidate(key);
  return useMutation({
    mutationFn: (id: string) => request<TrackerIssueDetail>(`${projectPath(key)}/tracker-issues/${enc(id)}/sync`, { method: 'POST' }),
    onSuccess: done,
  });
}

export function useUnmuteTrackerIssue(key: string) {
  const done = useInvalidate(key);
  return useMutation({
    mutationFn: (id: string) => request<UnmuteOutcome>(`${projectPath(key)}/tracker-issues/${enc(id)}/unmute`, { method: 'POST' }),
    onSuccess: done,
  });
}

export function useDismissTrackerIssue(key: string) {
  const done = useInvalidate(key);
  return useMutation({
    mutationFn: (id: string) => request<unknown>(`${projectPath(key)}/tracker-issues/${enc(id)}/dismiss`, { method: 'POST' }),
    onSuccess: done,
  });
}

export function useUntrackIssue(key: string) {
  const done = useInvalidate(key);
  return useMutation({
    mutationFn: (id: string) =>
      request<unknown>(`${projectPath(key)}/tracker-issues/${enc(id)}`, {
        method: 'DELETE',
      }),
    onSuccess: done,
  });
}

export function useTestTracker(key: string) {
  return useMutation({
    mutationFn: (trackerId: string) => request<TrackerTest>(`${projectPath(key)}/issue-trackers/${enc(trackerId)}/test`, { method: 'POST' }),
  });
}

export const TRACKER_LABEL: Record<string, string> = {
  jira: 'Jira',
  gitlab: 'GitLab',
  github: 'GitHub',
  azure: 'Azure DevOps',
};
