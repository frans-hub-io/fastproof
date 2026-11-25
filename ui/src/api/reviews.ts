// Case workflow and version review API. The DTOs are the generated `schema.gen.ts` types under the contract's names.
import { useInfiniteQuery, useMutation, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query';
import { useEffect, useRef } from 'react';
import { ApiError, projectPath, request, requestRaw, withQuery } from './client';
import { openEventStream, eventStreamSupported } from './sse';
import type { components } from './schema.gen';
import { PreconditionFailedError, etagOf, tmk, type Case } from './tm';

type S = components['schemas'];

export type CaseWorkflowView = S['CaseWorkflowView'];
export type CaseWorkflowSettings = S['CaseWorkflowSettings'];
export type CaseWorkflowStatus = S['CaseWorkflowStatus'];
export type CaseWorkflowFlow = S['CaseWorkflowFlow'];
export type CaseWorkflowTransition = S['CaseWorkflowTransition'];
export type CaseWorkflowPut = S['CaseWorkflowPut'];
export type CaseWorkflowSaved = S['CaseWorkflowSaved'];
export type AllowedTransition = S['AllowedTransition'];
export type ReviewThread = S['ReviewThread'];
export type ReviewVote = S['ReviewVote'];
export type ReviewVoteWrite = S['ReviewVoteWrite'];
export type ReviewQueue = S['ReviewQueue'];
export type ReviewQueueItem = S['ReviewQueueItem'];
export type ReviewBulk = S['ReviewBulk'];
export type ReviewBulkItem = S['ReviewBulkItem'];
export type ReviewBulkResult = S['ReviewBulkResult'];
export type ReviewBulkSkip = S['ReviewBulkSkip'];
export type CaseReviewSettings = S['CaseReviewSettings'];
export type ReviewKind = S['ReviewKind'];
export type ReviewState = S['ReviewState'];
export type ReviewDecision = S['ReviewDecision'];
export type ReviewMode = S['ReviewMode'];
export type WorkflowGuard = S['WorkflowGuard'];
export type WorkflowPermission = S['WorkflowPermission'];

export const rvk = {
  workflow: (key: string) => ['reviews', key, 'workflow'] as const,
  thread: (key: string, kind: ReviewKind, n: number) => ['reviews', key, 'thread', kind, n] as const,
  queue: (key: string, q?: ReviewQuery) => (q ? (['reviews', key, 'queue', q] as const) : (['reviews', key, 'queue'] as const)),
};

/** The review policy of a project that never configured one. */
export const DEFAULT_REVIEW: CaseReviewSettings = { mode: 'off', approvals: 1, resetOn: 'content', allowSelfApproval: false, imports: 'review' };

// Workflow

export function useCaseWorkflow(key: string, enabled = true) {
  return useQuery({
    queryKey: rvk.workflow(key),
    queryFn: () => request<CaseWorkflowView>(`${projectPath(key)}/case-workflow`),
    enabled,
    staleTime: 30_000,
    retry: false,
  });
}

/** `workflow_status_in_use`: states still used by cases (`details.statuses`). */
export interface StatusInUse {
  key: string;
  cases: number;
}

export function statusesInUse(e: unknown): StatusInUse[] | null {
  if (!(e instanceof ApiError) || e.status !== 409 || e.code !== 'workflow_status_in_use') return null;
  const d = e.details as { statuses?: unknown } | null | undefined;
  if (!d || !Array.isArray(d.statuses)) return [];
  return d.statuses.filter((s): s is StatusInUse => !!s && typeof (s as StatusInUse).key === 'string').map((s) => ({ key: s.key, cases: s.cases || 0 }));
}

function invalidateAfterWorkflow(qc: QueryClient, key: string): void {
  void qc.invalidateQueries({ queryKey: rvk.workflow(key) });
  void qc.invalidateQueries({ queryKey: ['reviews', key] });
  void qc.invalidateQueries({ queryKey: ['tm', key] });
  void qc.invalidateQueries({ queryKey: ['settings', key] });
}

export function useSaveCaseWorkflow(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: CaseWorkflowPut) => request<CaseWorkflowSaved>(`${projectPath(key)}/case-workflow`, { method: 'PUT', json: body }),
    onSuccess: (saved) => {
      qc.setQueryData(rvk.workflow(key), saved.view);
      invalidateAfterWorkflow(qc, key);
    },
  });
}

/** Saves `caseReview` through `PUT /settings`; the server keeps every area that is absent from the body. */
export function useSaveReviewPolicy(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (caseReview: CaseReviewSettings) => request<unknown>(`${projectPath(key)}/settings`, { method: 'PUT', json: { caseReview } }),
    onSuccess: () => invalidateAfterWorkflow(qc, key),
  });
}

// Transitions

function mapPrecondition(e: unknown): never {
  if (e instanceof ApiError && e.status === 412) throw new PreconditionFailedError(e);
  throw e;
}

/** `invalid_transition` details. `missing` holds `transition`, `permission`, `approved`, `requiredFields` or `notAuthor`. */
export interface TransitionRefusal {
  from: string | null;
  to: string | null;
  missing: string[];
}

export function transitionRefusal(e: unknown): TransitionRefusal | null {
  if (!(e instanceof ApiError) || e.status !== 409 || e.code !== 'invalid_transition') return null;
  const d = (e.details ?? {}) as Partial<TransitionRefusal>;
  return { from: typeof d.from === 'string' ? d.from : null, to: typeof d.to === 'string' ? d.to : null, missing: Array.isArray(d.missing) ? d.missing.map(String) : [] };
}

export function useTransitionCase(key: string, n: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async ({ to, etag }: { to: string; etag: string }) => {
      try {
        const r = await requestRaw(`${projectPath(key)}/cases/${String(n)}/transition`, { method: 'POST', json: { to }, headers: { 'If-Match': etag } });
        const value = r.data as Case;
        return { value, etag: r.headers.get('ETag') ?? etagOf(value.revision) };
      } catch (e) {
        return mapPrecondition(e);
      }
    },
    onSuccess: (v) => {
      qc.setQueryData(tmk.case(key, n), v);
      void qc.invalidateQueries({ queryKey: rvk.thread(key, 'case', n) });
      void qc.invalidateQueries({ queryKey: tmk.versions(key, n) });
      void qc.invalidateQueries({ queryKey: tmk.cases(key) });
      void qc.invalidateQueries({ queryKey: rvk.queue(key) });
    },
  });
}

// Threads and votes

/** A thread with the `ETag` (the case revision) a vote sends as `If-Match`. */
export interface ThreadRead {
  thread: ReviewThread;
  etag: string | null;
}

function threadPath(key: string, kind: ReviewKind, n: number): string {
  return `${projectPath(key)}/${kind === 'case' ? 'cases' : 'shared-steps'}/${String(n)}/reviews`;
}

export async function fetchThread(key: string, kind: ReviewKind, n: number): Promise<ThreadRead> {
  const r = await requestRaw(threadPath(key, kind, n));
  return { thread: r.data as ReviewThread, etag: r.headers.get('ETag') };
}

export function useReviewThread(key: string, kind: ReviewKind, n: number, enabled = true) {
  return useQuery({
    queryKey: rvk.thread(key, kind, n),
    queryFn: () => fetchThread(key, kind, n),
    enabled: enabled && Number.isInteger(n) && n > 0,
    staleTime: 0,
    retry: false,
  });
}

/** `version_moved`: the head changed after the reviewer read it. */
export function versionMoved(e: unknown): { currentVersion: number | null } | null {
  if (!(e instanceof ApiError) || e.status !== 409 || e.code !== 'version_moved') return null;
  const d = (e.details ?? {}) as { currentVersion?: unknown };
  return { currentVersion: typeof d.currentVersion === 'number' ? d.currentVersion : null };
}

export function useVote(key: string, kind: ReviewKind, n: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async ({ body, etag }: { body: ReviewVoteWrite; etag: string | null }): Promise<ThreadRead> => {
      try {
        const r = await requestRaw(threadPath(key, kind, n), { method: 'POST', json: body, headers: etag ? { 'If-Match': etag } : undefined });
        return { thread: r.data as ReviewThread, etag: r.headers.get('ETag') };
      } catch (e) {
        return mapPrecondition(e);
      }
    },
    onSuccess: (r) => {
      qc.setQueryData(rvk.thread(key, kind, n), r);
      void qc.invalidateQueries({ queryKey: rvk.queue(key) });
      if (kind === 'case') {
        void qc.invalidateQueries({ queryKey: tmk.case(key, n) });
        void qc.invalidateQueries({ queryKey: tmk.cases(key) });
      }
    },
  });
}

// Queue

export interface ReviewQuery {
  kind?: ReviewKind;
  state?: ReviewState;
  reviewer?: 'me';
  author?: 'me';
  folder?: string;
}

export const QUEUE_PAGE = 50;

export function useReviewQueue(key: string, q: ReviewQuery, enabled = true) {
  return useInfiniteQuery({
    queryKey: rvk.queue(key, q),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => request<ReviewQueue>(withQuery(`${projectPath(key)}/reviews`, { ...q, before: pageParam, limit: QUEUE_PAGE })),
    getNextPageParam: (last) => last.next ?? undefined,
    enabled,
    retry: false,
  });
}

/** The whole queue of a filter (pages read until the end), for the batch review. */
export async function fetchWholeQueue(key: string, q: ReviewQuery): Promise<ReviewQueueItem[]> {
  const out: ReviewQueueItem[] = [];
  let before: string | undefined;
  for (let i = 0; i < 40; i++) {
    const page = await request<ReviewQueue>(withQuery(`${projectPath(key)}/reviews`, { ...q, before, limit: 500 }));
    out.push(...page.items);
    if (!page.next) break;
    before = page.next;
  }
  return out;
}

export function useBulkReview(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: ReviewBulk) => request<ReviewBulkResult>(`${projectPath(key)}/reviews/bulk`, { method: 'POST', json: body }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ['reviews', key] });
      void qc.invalidateQueries({ queryKey: ['tm', key] });
    },
  });
}

/** Numbers of the cases an FPQL query selects, or `null` when the server has no queries yet. */
export async function fetchQueryNumbers(key: string, query: string): Promise<number[] | null> {
  const out: number[] = [];
  let before: string | undefined;
  for (let i = 0; i < 40; i++) {
    let page: { items: { number: number }[]; next?: string | null };
    try {
      page = await request<{ items: { number: number }[]; next?: string | null }>(withQuery(`${projectPath(key)}/cases`, { query, before, limit: 500 }));
    } catch (e) {
      if (e instanceof ApiError && e.status === 400 && e.code === 'invalid_query' && /not available/i.test(e.message)) return null;
      throw e;
    }
    out.push(...page.items.map((c) => c.number));
    if (!page.next) break;
    before = page.next;
  }
  return out;
}

// Live events

export interface ReviewedEvent {
  number: number;
  version?: number;
  reviewState?: ReviewState | null;
  approvedVersion?: number | null;
}

function parse(data: string): ReviewedEvent | null {
  try {
    return JSON.parse(data) as ReviewedEvent;
  } catch {
    return null;
  }
}

/** Refreshes threads and the queue on `case.reviewed` and `sharedStep.reviewed`. */
export function useReviewEvents(key: string | undefined, onEvent?: (kind: ReviewKind, e: ReviewedEvent) => void): void {
  const qc = useQueryClient();
  const cb = useRef(onEvent);
  useEffect(() => {
    cb.current = onEvent;
  });
  useEffect(() => {
    if (!key || !eventStreamSupported()) return;
    const handle = (kind: ReviewKind) => (d: string) => {
      const e = parse(d);
      if (!e) return;
      void qc.invalidateQueries({ queryKey: rvk.thread(key, kind, e.number) });
      void qc.invalidateQueries({ queryKey: rvk.queue(key) });
      if (kind === 'case') void qc.invalidateQueries({ queryKey: tmk.case(key, e.number) });
      cb.current?.(kind, e);
    };
    const es = openEventStream(`${projectPath(key)}/events`, { onOpen: () => undefined, onError: () => undefined, events: { 'case.reviewed': handle('case'), 'sharedStep.reviewed': handle('sharedStep') } });
    return () => es.close();
  }, [key, qc]);
}

export const REVIEW_STATE_LABEL: Record<ReviewState, string> = { pending: 'Waiting for review', changes_requested: 'Changes requested', approved: 'Approved' };
export const REVIEW_STATE_COLOR: Record<ReviewState, string> = { pending: 'yellow', changes_requested: 'red', approved: 'teal' };

export const WHY_NOT: Record<string, string> = {
  permission: 'You need the cases:review permission to vote.',
  review_not_enabled: 'Review is off for this project.',
  version_moved: 'The case changed since this page loaded; reload to review the latest version.',
  not_pending: 'This version has already been decided.',
  ai_self_review: 'An AI draft cannot be approved by the account that produced it.',
  self_approval: 'You cannot approve your own change; another reviewer has to.',
};

export const SKIP_LABEL: Record<string, string> = {
  version_moved: 'changed after you opened it',
  self_approval: 'your own change',
  ai_self_review: 'AI draft by this account',
  not_pending: 'already decided',
  review_not_enabled: 'review is off',
  not_found: 'not found',
};

export const MISSING_LABEL: Record<string, string> = {
  transition: 'no such transition',
  permission: 'you lack the permission this move needs',
  approved: 'needs an approved version',
  requiredFields: 'required fields are empty',
  notAuthor: 'the author cannot do this',
};
