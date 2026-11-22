// Test management API (stage 2a). The DTOs are the generated `schema.gen.ts` types under the contract's names.
import { keepPreviousData, useInfiniteQuery, useMutation, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query';
import { useEffect, useRef, useState } from 'react';
import { apiErrorFrom, apiFetch } from '@franshub/ui-shell';
import { ApiError, projectPath, request, requestRaw, withQuery, type Query } from './client';
import { openEventStream, eventStreamSupported } from './sse';
import type { components } from './schema.gen';
import type { Launch, Page } from './types';

type S = components['schemas'];

export type CaseStatus = S['CaseStatus'];
export type CaseOrigin = S['CaseOrigin'];
export type CaseStep = S['CaseStep'];
export type CaseLink = S['CaseLink'];
export type CaseBody = S['CaseBody'];
export type CaseStepHtml = S['CaseStepHtml'];
export type CaseHtml = S['CaseHtml'];
export type CaseWrite = S['CaseWrite'];
export type FieldValue = S['FieldValue'];
export type CasePatch = S['CasePatch'];
export type LastResult = S['LastResult'];
export type CaseSummary = S['CaseSummary'];
export type CaseIdentity = S['CaseIdentity'];
export type CaseKey = S['CaseKey'];
export type Case = S['Case'];
export type CaseList = S['CaseList'];
export type FolderCounts = S['FolderCounts'];
export type CaseFolder = S['CaseFolder'];
export type CaseFolderWrite = S['CaseFolderWrite'];
export type CaseFolderPatch = S['CaseFolderPatch'];
export type CaseFolderList = S['CaseFolderList'];
export type CaseFolderRef = S['CaseFolderRef'];
export type CaseTreeNode = S['CaseTreeNode'];
export type CaseTree = S['CaseTree'];
export type FieldOption = S['FieldOption'];
export type FieldType = S['FieldType'];
export type CaseFieldDef = S['CaseFieldDef'];
export type CaseFieldWrite = S['CaseFieldWrite'];
export type CaseFieldPatch = S['CaseFieldPatch'];
export type CaseFieldList = S['CaseFieldList'];
export type VersionSource = S['VersionSource'];
export type CaseVersionSummary = S['CaseVersionSummary'];
export type CaseVersionList = S['CaseVersionList'];
export type CaseSnapshot = S['CaseSnapshot'];
export type CaseVersion = S['CaseVersion'];
export type CaseHistoryEntry = S['CaseHistoryEntry'];
export type CaseHistoryList = S['CaseHistoryList'];
export type TestIdentity = S['TestIdentity'];
export type TestIdentityList = S['TestIdentityList'];
export type CaseBulkAction = S['CaseBulkAction'];
export type CaseBulk = S['CaseBulk'];
export type CaseBulkResult = S['CaseBulkResult'];
export type CaseFilter = S['CaseFilter'];
export type CaseProposal = S['CaseProposal'];
export type CaseSuggestion = S['CaseSuggestion'];
export type CaseSuggestionList = S['CaseSuggestionList'];
export type CaseSuggestionAccepted = S['CaseSuggestionAccepted'];
export type AttachmentRef = S['AttachmentRef'];
export type RunStatus = S['RunStatus'];
export type StepStatus = S['StepStatus'];
export type TestPlan = S['TestPlan'];
export type TestPlanWrite = S['TestPlanWrite'];
export type TestPlanPatch = S['TestPlanPatch'];
export type TestPlanItem = S['TestPlanItem'];
export type TestPlanItemsChange = S['TestPlanItemsChange'];
export type PlanSyncDiff = S['PlanSyncDiff'];
export type StartPlanLaunch = S['StartPlanLaunch'];
export type PlanLaunch = S['PlanLaunch'];
export type LaunchManual = S['LaunchManual'];
export type RunItemSummary = S['RunItemSummary'];
export type MyRunItem = S['MyRunItem'];
export type RunStepResult = S['RunStepResult'];
export type RunItem = S['RunItem'];
export type RunStepWrite = S['RunStepWrite'];
export type RunStepAck = S['RunStepAck'];
export type RunItemPatch = S['RunItemPatch'];
export type RunItemsAdd = S['RunItemsAdd'];
export type RunItemsAdded = S['RunItemsAdded'];
export type RunDefect = S['RunDefect'];
export type ManualClose = S['ManualClose'];
export type TmJob = S['TmJob'];
export type ImportPreview = S['ImportPreview'];
export type ImportFormat = S['ImportFormat'];
export type ImportOptions = S['ImportOptions'];
export type TestOpsImport = S['TestOpsImport'];
export type ImportReport = S['ImportReport'];
export type MarkdownHtml = S['MarkdownHtml'];
export type NewIssueUrl = S['NewIssueUrl'];

/** `Launch` with the stage 2a additions (now part of the generated `Launch`). */
export type TmLaunch = Launch;
/** The stage 2a fields of a result row. */
export type ResultSourceInfo = Pick<S['ResultSummary'], 'source' | 'caseNumber' | 'runItemId'>;

/** `details` of a 412 `precondition_failed`. */
export interface ConflictDetails {
  currentRevision: number;
  changedBy: string | null;
  source: string;
}

/** A write was refused because the resource changed since it was read (HTTP 412). */
export class PreconditionFailedError extends ApiError {
  readonly conflict: ConflictDetails;
  constructor(err: ApiError) {
    super(412, { code: err.code, message: err.message, details: err.details });
    const d = (err.details ?? {}) as Partial<ConflictDetails>;
    this.conflict = {
      currentRevision: typeof d.currentRevision === 'number' ? d.currentRevision : -1,
      changedBy: typeof d.changedBy === 'string' ? d.changedBy : null,
      source: typeof d.source === 'string' ? d.source : 'ui',
    };
  }
}

export function isConflict(e: unknown): e is PreconditionFailedError {
  return e instanceof PreconditionFailedError;
}

/** A value read together with its `ETag`. */
export interface Versioned<T> {
  value: T;
  etag: string;
}

export function etagOf(revision: number): string {
  return `"r${String(revision)}"`;
}

export function revisionOf(etag: string | null | undefined): number | null {
  const m = /^(?:W\/)?"r(\d+)"$/.exec(etag ?? '');
  return m ? Number(m[1]) : null;
}

function mapConflict(e: unknown): never {
  if (e instanceof ApiError && e.status === 412) throw new PreconditionFailedError(e);
  throw e;
}

/** Reads a resource with its `ETag` (falls back to the body's `revision`). */
export async function getVersioned<T extends { revision: number }>(path: string): Promise<Versioned<T>> {
  const r = await requestRaw(path);
  const value = r.data as T;
  return { value, etag: r.headers.get('ETag') ?? etagOf(value.revision) };
}

/** Writes with `If-Match`; a 412 becomes a `PreconditionFailedError` carrying the server's `details`. */
export async function writeVersioned<T extends { revision: number }>(path: string, method: 'PUT' | 'PATCH' | 'POST', etag: string, json?: unknown): Promise<Versioned<T>> {
  try {
    const r = await requestRaw(path, { method, json, headers: { 'If-Match': etag } });
    const value = r.data as T;
    return { value, etag: r.headers.get('ETag') ?? etagOf(value.revision) };
  } catch (e) {
    return mapConflict(e);
  }
}

const enc = encodeURIComponent;

export const tmk = {
  all: (key: string) => ['tm', key] as const,
  cases: (key: string, q?: CaseQuery) => (q ? (['tm', key, 'cases', q] as const) : (['tm', key, 'cases'] as const)),
  caseIndex: (key: string) => ['tm', key, 'caseIndex'] as const,
  tree: (key: string, groupBy: string, parent: string | null) => ['tm', key, 'tree', groupBy, parent] as const,
  folders: (key: string) => ['tm', key, 'folders'] as const,
  fields: (key: string) => ['tm', key, 'fields'] as const,
  case: (key: string, n: number) => ['tm', key, 'case', n] as const,
  versions: (key: string, n: number) => ['tm', key, 'case', n, 'versions'] as const,
  version: (key: string, n: number, v: number) => ['tm', key, 'case', n, 'version', v] as const,
  history: (key: string, n: number, f?: object) => (f ? (['tm', key, 'case', n, 'history', f] as const) : (['tm', key, 'case', n, 'history'] as const)),
  suggestions: (key: string, n: number) => ['tm', key, 'case', n, 'suggestions'] as const,
  identities: (key: string, q?: object) => (q ? (['tm', key, 'identities', q] as const) : (['tm', key, 'identities'] as const)),
  plans: (key: string, q?: object) => (q ? (['tm', key, 'plans', q] as const) : (['tm', key, 'plans'] as const)),
  plan: (key: string, id: string) => ['tm', key, 'plan', id] as const,
  planItems: (key: string, id: string) => ['tm', key, 'plan', id, 'items'] as const,
  runItems: (key: string, launchId: string, q?: object) => (q ? (['tm', key, 'run', launchId, q] as const) : (['tm', key, 'run', launchId] as const)),
  runItem: (key: string, id: string) => ['tm', key, 'runItem', id] as const,
  myRunItems: (key: string) => ['tm', key, 'myRunItems'] as const,
  job: (key: string, id: string) => ['tm', key, 'job', id] as const,
  launch: (key: string, id: string) => ['launch', key, id] as const,
};

function invalidateCases(qc: QueryClient, key: string): void {
  void qc.invalidateQueries({ queryKey: tmk.cases(key) });
  void qc.invalidateQueries({ queryKey: ['tm', key, 'tree'] });
  void qc.invalidateQueries({ queryKey: tmk.folders(key) });
  void qc.invalidateQueries({ queryKey: tmk.caseIndex(key) });
}

// Cases

export interface CaseQuery {
  folder?: string;
  recursive?: boolean;
  q?: string;
  status?: string;
  automation?: 'manual' | 'automated';
  field?: string[];
  tag?: string[];
  staleDays?: number;
  aiUnreviewed?: boolean;
  archived?: boolean;
  sort?: string;
}

export const CASE_PAGE = 100;

/** `withQuery` plus repeated parameters (`field=k:v&field=…`). */
export function withMulti(path: string, query: Query, multi: Record<string, string[] | undefined>): string {
  const base = withQuery(path, query);
  const extra = new URLSearchParams();
  for (const [k, list] of Object.entries(multi)) for (const v of list ?? []) extra.append(k, v);
  const s = extra.toString();
  if (!s) return base;
  return base.includes('?') ? `${base}&${s}` : `${base}?${s}`;
}

export function casesUrl(key: string, q: CaseQuery, extra: Query = {}, suffix = ''): string {
  const { field, tag, recursive, aiUnreviewed, archived, ...rest } = q;
  return withMulti(
    `${projectPath(key)}/cases${suffix}`,
    { ...rest, recursive: recursive === undefined ? undefined : String(recursive), aiUnreviewed: aiUnreviewed ? 'true' : undefined, archived: archived ? 'true' : undefined, ...extra },
    { field, tag },
  );
}

export function useCases(key: string, q: CaseQuery, enabled = true) {
  return useInfiniteQuery({
    queryKey: tmk.cases(key, q),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => request<CaseList>(casesUrl(key, q, { before: pageParam, limit: CASE_PAGE })),
    getNextPageParam: (last) => last.next ?? undefined,
    placeholderData: keepPreviousData,
    enabled,
  });
}

/** The first cases of the project for the command palette (number and name). */
export function useCaseIndex(key: string | undefined, enabled: boolean) {
  return useQuery({
    queryKey: tmk.caseIndex(key ?? ''),
    queryFn: () => request<CaseList>(casesUrl(key ?? '', {}, { limit: 500 })).then((r) => r.items),
    enabled: !!key && enabled,
    staleTime: 60_000,
    retry: false,
  });
}

export function useCaseTree(key: string, groupBy: 'folder' | 'behavior' | 'suite', parent: string | null, enabled = true) {
  return useQuery({
    queryKey: tmk.tree(key, groupBy, parent),
    queryFn: () => request<CaseTree>(withQuery(`${projectPath(key)}/cases/tree`, { groupBy, parent })).then((r) => r.nodes),
    enabled,
    staleTime: 30_000,
  });
}

export function useCase(key: string, n: number) {
  return useQuery({
    queryKey: tmk.case(key, n),
    queryFn: () => getVersioned<Case>(`${projectPath(key)}/cases/${String(n)}`),
    enabled: Number.isInteger(n) && n > 0,
    staleTime: 0,
  });
}

export function useCreateCase(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: CaseWrite) => request<Case>(`${projectPath(key)}/cases`, { method: 'POST', json: body }),
    onSuccess: () => invalidateCases(qc, key),
  });
}

function afterCaseWrite(qc: QueryClient, key: string, n: number, v: Versioned<Case>): void {
  qc.setQueryData(tmk.case(key, n), v);
  void qc.invalidateQueries({ queryKey: tmk.versions(key, n) });
  invalidateCases(qc, key);
}

export function useUpdateCase(key: string, n: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ body, etag }: { body: CaseWrite; etag: string }) => writeVersioned<Case>(`${projectPath(key)}/cases/${String(n)}`, 'PUT', etag, body),
    onSuccess: (v) => afterCaseWrite(qc, key, n, v),
  });
}

export function usePatchCase(key: string, n: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ patch, etag }: { patch: CasePatch; etag: string }) => writeVersioned<Case>(`${projectPath(key)}/cases/${String(n)}`, 'PATCH', etag, patch),
    onSuccess: (v) => afterCaseWrite(qc, key, n, v),
  });
}

/** "Reviewed": clears the AI-draft mark of a case (`POST /cases/{n}/review` with `If-Match`). */
export function useReviewCase(key: string, n: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (etag: string) => writeVersioned<Case>(`${projectPath(key)}/cases/${String(n)}/review`, 'POST', etag),
    onSuccess: (v) => afterCaseWrite(qc, key, n, v),
  });
}

export function useArchiveCase(key: string, n: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (restore: boolean) =>
      restore ? request<unknown>(`${projectPath(key)}/cases/${String(n)}/restore`, { method: 'POST' }) : request<unknown>(`${projectPath(key)}/cases/${String(n)}`, { method: 'DELETE' }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: tmk.case(key, n) });
      invalidateCases(qc, key);
    },
  });
}

export function useBulkCases(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: CaseBulk) => request<CaseBulkResult>(`${projectPath(key)}/cases/bulk`, { method: 'POST', json: body }),
    onSuccess: () => {
      invalidateCases(qc, key);
      void qc.invalidateQueries({ predicate: (q) => q.queryKey[0] === 'tm' && q.queryKey[1] === key && q.queryKey[2] === 'case' });
    },
  });
}

export function useCaseVersions(key: string, n: number, enabled = true) {
  return useQuery({
    queryKey: tmk.versions(key, n),
    queryFn: () => request<Page<CaseVersionSummary>>(withQuery(`${projectPath(key)}/cases/${String(n)}/versions`, { limit: 500 })).then((r) => r.items),
    enabled,
  });
}

export function fetchCaseVersion(key: string, n: number, v: number): Promise<CaseVersion> {
  return request<CaseVersion>(`${projectPath(key)}/cases/${String(n)}/versions/${String(v)}`);
}

export function useCaseVersion(key: string, n: number, v: number | null) {
  return useQuery({
    queryKey: tmk.version(key, n, v ?? 0),
    queryFn: () => fetchCaseVersion(key, n, v ?? 0),
    enabled: v !== null && v > 0,
    staleTime: Infinity,
  });
}

export function useRestoreCaseVersion(key: string, n: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ version, etag }: { version: number; etag: string }) => writeVersioned<Case>(`${projectPath(key)}/cases/${String(n)}/versions/${String(version)}/restore`, 'POST', etag),
    onSuccess: (v) => afterCaseWrite(qc, key, n, v),
  });
}

export interface CaseHistoryFilters {
  branch?: string;
  environment?: string;
  source?: 'manual' | 'automated';
}

export function useCaseHistory(key: string, n: number, f: CaseHistoryFilters, enabled = true) {
  return useQuery({
    queryKey: tmk.history(key, n, f),
    queryFn: () => request<{ items: CaseHistoryEntry[] }>(withQuery(`${projectPath(key)}/cases/${String(n)}/history`, { ...f, limit: 100 })).then((r) => r.items),
    enabled,
  });
}

export function useLinkIdentity(key: string, n: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ testCaseId, unlink }: { testCaseId: string; unlink?: boolean }) =>
      unlink
        ? request<unknown>(`${projectPath(key)}/cases/${String(n)}/identities/${enc(testCaseId)}`, { method: 'DELETE' })
        : request<unknown>(`${projectPath(key)}/cases/${String(n)}/identities`, { method: 'POST', json: { testCaseId } }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: tmk.case(key, n) });
      void qc.invalidateQueries({ queryKey: tmk.identities(key) });
      invalidateCases(qc, key);
    },
  });
}

export function useTestIdentities(key: string, q: { unlinked?: boolean; branch?: string; q?: string }, enabled = true) {
  return useInfiniteQuery({
    queryKey: tmk.identities(key, q),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      request<Page<TestIdentity>>(withQuery(`${projectPath(key)}/test-identities`, { unlinked: q.unlinked ? 'true' : undefined, branch: q.branch, q: q.q, before: pageParam, limit: 100 })),
    getNextPageParam: (last) => last.next ?? undefined,
    enabled,
  });
}

/** A file name as an HTTP header value (printable ASCII; other characters become `_`). */
export function headerSafe(name: string): string {
  return name.replace(/[^\x20-\x7e]/g, '_').slice(0, 255);
}

/** Uploads one file as a case attachment. */
export function uploadCaseAttachment(key: string, n: number, file: Blob, name: string): Promise<AttachmentRef> {
  return request<Omit<AttachmentRef, 'name' | 'contentType'> & Partial<AttachmentRef>>(`${projectPath(key)}/cases/${String(n)}/attachments`, {
    method: 'POST',
    body: file,
    headers: { 'Content-Type': file.type || 'application/octet-stream', 'X-FastProof-Attachment-Name': headerSafe(name) },
  }).then((r) => ({ name, contentType: file.type || 'application/octet-stream', ...r }));
}

export function markdownPreview(key: string, markdown: string, signal?: AbortSignal): Promise<string> {
  return request<{ html: string }>(`${projectPath(key)}/markdown/preview`, { method: 'POST', json: { markdown }, signal }).then((r) => r.html);
}

// Folders and fields

/** Every folder of the project in one request (`GET /case-folders?all=true`, parents first, capped by the server at `maxFolders`). */
export async function fetchAllFolders(key: string): Promise<CaseFolder[]> {
  const r = await request<CaseFolderList>(withQuery(`${projectPath(key)}/case-folders`, { all: true }));
  return r.items;
}

export function useCaseFolders(key: string) {
  return useQuery({
    queryKey: tmk.folders(key),
    queryFn: () => fetchAllFolders(key),
    staleTime: 30_000,
  });
}

export function useSaveFolder(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id?: string; body: Partial<CaseFolderWrite> }) =>
      id
        ? request<CaseFolder>(`${projectPath(key)}/case-folders/${enc(id)}`, { method: 'PATCH', json: body })
        : request<CaseFolder>(`${projectPath(key)}/case-folders`, { method: 'POST', json: body }),
    onSuccess: () => invalidateCases(qc, key),
  });
}

export function useDeleteFolder(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, mode }: { id: string; mode?: 'moveUp' | 'archiveCases' }) => request<unknown>(withQuery(`${projectPath(key)}/case-folders/${enc(id)}`, { mode }), { method: 'DELETE' }),
    onSuccess: () => invalidateCases(qc, key),
  });
}

export function useCaseFields(key: string) {
  return useQuery({
    queryKey: tmk.fields(key),
    queryFn: () => request<{ items: CaseFieldDef[] }>(`${projectPath(key)}/case-fields`).then((r) => r.items),
    staleTime: 60_000,
  });
}

export function useSaveCaseField(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, body }: { id?: string; body: Partial<CaseFieldWrite> }) =>
      id
        ? request<CaseFieldDef>(`${projectPath(key)}/case-fields/${enc(id)}`, { method: 'PATCH', json: body })
        : request<CaseFieldDef>(`${projectPath(key)}/case-fields`, { method: 'POST', json: body }),
    onSuccess: () => qc.invalidateQueries({ queryKey: tmk.fields(key) }),
  });
}

export function useDeleteCaseField(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => request<unknown>(`${projectPath(key)}/case-fields/${enc(id)}`, { method: 'DELETE' }),
    onSuccess: () => qc.invalidateQueries({ queryKey: tmk.fields(key) }),
  });
}

// Suggestions

export function useCaseSuggestions(key: string, n: number) {
  return useQuery({
    queryKey: tmk.suggestions(key, n),
    queryFn: () => request<{ items: CaseSuggestion[] }>(`${projectPath(key)}/cases/${String(n)}/suggestions`).then((r) => r.items),
  });
}

export function useDecideSuggestion(key: string, n: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async ({ id, accept, etag }: { id: string; accept: boolean; etag: string }) => {
      if (!accept) return request<CaseSuggestion>(`${projectPath(key)}/case-suggestions/${enc(id)}/reject`, { method: 'POST' });
      try {
        const r = await requestRaw(`${projectPath(key)}/case-suggestions/${enc(id)}/accept`, { method: 'POST', headers: { 'If-Match': etag } });
        return r.data as CaseSuggestion;
      } catch (e) {
        return mapConflict(e);
      }
    },
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: tmk.suggestions(key, n) });
      void qc.invalidateQueries({ queryKey: tmk.case(key, n) });
      void qc.invalidateQueries({ queryKey: tmk.versions(key, n) });
    },
  });
}

// Plans

export function useManagedPlans(key: string, q: { q?: string; archived?: boolean }) {
  return useQuery({
    queryKey: tmk.plans(key, q),
    queryFn: () => request<{ items: TestPlan[] }>(withQuery(`${projectPath(key)}/test-plans`, { kind: 'managed', q: q.q, archived: q.archived ? 'true' : undefined })).then((r) => r.items),
  });
}

export function useCreateManagedPlan(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: TestPlanWrite) => request<{ id: string }>(`${projectPath(key)}/test-plans`, { method: 'POST', json: { kind: 'managed', ...body } }),
    onSuccess: () => qc.invalidateQueries({ queryKey: tmk.plans(key) }),
  });
}

export function useTestPlan(key: string, id: string) {
  return useQuery({
    queryKey: tmk.plan(key, id),
    queryFn: () => getVersioned<TestPlan>(`${projectPath(key)}/test-plans/${enc(id)}`),
    enabled: !!id,
  });
}

export function useUpdateTestPlan(key: string, id: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ patch, etag }: { patch: Partial<TestPlanWrite>; etag: string }) => writeVersioned<TestPlan>(`${projectPath(key)}/test-plans/${enc(id)}`, 'PATCH', etag, patch),
    onSuccess: (v) => {
      qc.setQueryData(tmk.plan(key, id), v);
      void qc.invalidateQueries({ queryKey: tmk.plans(key) });
    },
  });
}

export function useArchiveTestPlan(key: string, id: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: () => request<unknown>(`${projectPath(key)}/test-plans/${enc(id)}`, { method: 'DELETE' }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: tmk.plans(key) });
      void qc.invalidateQueries({ queryKey: tmk.plan(key, id) });
    },
  });
}

/** Every item of a plan (pages read until the end; a plan holds at most `max_plan_cases`). */
export function useTestPlanItems(key: string, id: string) {
  return useQuery({
    queryKey: tmk.planItems(key, id),
    queryFn: async () => {
      const out: TestPlanItem[] = [];
      let before: string | undefined;
      for (let i = 0; i < 200; i++) {
        const page = await request<Page<TestPlanItem>>(withQuery(`${projectPath(key)}/test-plans/${enc(id)}/items`, { before, limit: 500 }));
        out.push(...page.items);
        if (!page.next) break;
        before = page.next;
      }
      return out;
    },
    enabled: !!id,
  });
}

export function useChangeTestPlanItems(key: string, id: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: TestPlanItemsChange) => request<unknown>(`${projectPath(key)}/test-plans/${enc(id)}/items`, { method: 'POST', json: body }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: tmk.planItems(key, id) });
      void qc.invalidateQueries({ queryKey: tmk.plan(key, id) });
      void qc.invalidateQueries({ queryKey: tmk.plans(key) });
    },
  });
}

export function useSyncTestPlan(key: string, id: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (dryRun: boolean) => request<PlanSyncDiff>(withQuery(`${projectPath(key)}/test-plans/${enc(id)}/sync`, { dryRun: String(dryRun) }), { method: 'POST' }),
    onSuccess: (_d, dryRun) => {
      if (dryRun) return;
      void qc.invalidateQueries({ queryKey: tmk.planItems(key, id) });
      void qc.invalidateQueries({ queryKey: tmk.plan(key, id) });
      void qc.invalidateQueries({ queryKey: tmk.plans(key) });
    },
  });
}

export function planTestplanUrl(key: string, id: string): string {
  return `${projectPath(key)}/test-plans/${enc(id)}/testplan.json`;
}

export function launchTestplanUrl(key: string, launchId: string): string {
  return `${projectPath(key)}/launches/${enc(launchId)}/testplan.json`;
}

export function useStartPlanLaunch(key: string, id: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: StartPlanLaunch) => request<PlanLaunch>(`${projectPath(key)}/test-plans/${enc(id)}/launches`, { method: 'POST', json: body }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ['launches', key] });
      void qc.invalidateQueries({ queryKey: tmk.myRunItems(key) });
    },
  });
}

// Manual runs

export function useAddRunItems(key: string, launchId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: RunItemsAdd) => request<RunItemsAdded>(`${projectPath(key)}/launches/${enc(launchId)}/run-items`, { method: 'POST', json: body }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: tmk.runItems(key, launchId) });
      void qc.invalidateQueries({ queryKey: tmk.launch(key, launchId) });
    },
  });
}

export interface RunItemQuery {
  status?: string;
  assignee?: string;
  folder?: string;
}

/** Every run item of a launch that matches (pages read until the end). */
export function useRunItems(key: string, launchId: string, q: RunItemQuery, enabled = true) {
  return useQuery({
    queryKey: tmk.runItems(key, launchId, q),
    queryFn: async () => {
      const out: RunItemSummary[] = [];
      let before: string | undefined;
      for (let i = 0; i < 200; i++) {
        const page = await request<Page<RunItemSummary>>(withQuery(`${projectPath(key)}/launches/${enc(launchId)}/run-items`, { ...q, before, limit: 500 }));
        out.push(...page.items);
        if (!page.next) break;
        before = page.next;
      }
      return out;
    },
    enabled: !!launchId && enabled,
    placeholderData: keepPreviousData,
  });
}

export function useMyRunItems(key: string) {
  return useQuery({
    queryKey: tmk.myRunItems(key),
    queryFn: () => request<Page<RunItemSummary & { launchId?: string }>>(withQuery(`${projectPath(key)}/run-items`, { assignee: 'me', status: 'untested,in_progress', limit: 100 })).then((r) => r.items),
  });
}

export function useRunItem(key: string, id: string | undefined) {
  return useQuery({
    queryKey: tmk.runItem(key, id ?? ''),
    queryFn: () => getVersioned<RunItem>(`${projectPath(key)}/run-items/${enc(id ?? '')}`),
    enabled: !!id,
    staleTime: 0,
  });
}

export function putRunStep(key: string, itemId: string, path: string, body: RunStepWrite): Promise<RunStepAck> {
  return request<RunStepAck>(`${projectPath(key)}/run-items/${enc(itemId)}/steps/${enc(path)}`, { method: 'PUT', json: body });
}

export function patchRunItem(key: string, itemId: string, etag: string, patch: RunItemPatch): Promise<Versioned<RunItem>> {
  return writeVersioned<RunItem>(`${projectPath(key)}/run-items/${enc(itemId)}`, 'PATCH', etag, patch);
}

export function uploadRunAttachment(key: string, itemId: string, file: Blob, name: string): Promise<AttachmentRef> {
  return request<Partial<AttachmentRef> & { id: string }>(`${projectPath(key)}/run-items/${enc(itemId)}/attachments`, {
    method: 'POST',
    body: file,
    headers: { 'Content-Type': file.type || 'application/octet-stream', 'X-FastProof-Attachment-Name': headerSafe(name) },
  }).then((r) => ({ sha256: '', size: file.size, contentType: file.type || 'application/octet-stream', name, ...r }));
}

export function useRunItemDefect(key: string, itemId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: RunDefect) => request<unknown>(`${projectPath(key)}/run-items/${enc(itemId)}/defect`, { method: 'POST', json: body }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: tmk.runItem(key, itemId) });
      void qc.invalidateQueries({ queryKey: ['results', key] });
    },
  });
}

export function fetchNewIssueUrl(key: string, itemId: string, tracker?: string): Promise<string> {
  return request<{ url: string }>(withQuery(`${projectPath(key)}/run-items/${enc(itemId)}/new-issue-url`, { tracker })).then((r) => r.url);
}

export function rerunRunItem(key: string, itemId: string): Promise<RunItem> {
  return request<RunItem>(`${projectPath(key)}/run-items/${enc(itemId)}/rerun`, { method: 'POST' });
}

export function useManualRunState(key: string, launchId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ close, untestedTo }: { close: boolean; untestedTo?: ManualClose['untestedTo'] }) =>
      close
        ? request<unknown>(`${projectPath(key)}/launches/${enc(launchId)}/manual/close`, { method: 'POST', json: { untestedTo: untestedTo ?? 'leave' } })
        : request<unknown>(`${projectPath(key)}/launches/${enc(launchId)}/manual/reopen`, { method: 'POST' }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: tmk.launch(key, launchId) });
      void qc.invalidateQueries({ queryKey: tmk.runItems(key, launchId) });
    },
  });
}

export function manualReportUrl(key: string, launchId: string, format: 'csv' | 'json'): string {
  return withQuery(`${projectPath(key)}/launches/${enc(launchId)}/manual-report`, { format });
}

// Import and export

export function previewImport(key: string, file: File): Promise<ImportPreview> {
  const form = new FormData();
  form.append('file', file);
  return request<ImportPreview>(`${projectPath(key)}/imports/preview`, { method: 'POST', body: form });
}

/** Starts an import (always a dry run first): multipart `options` (JSON), `file` and optionally `attachments` (a zip). */
export function createImport(key: string, file: File, options: ImportOptions, attachments?: File | null): Promise<TmJob> {
  const form = new FormData();
  form.append('options', JSON.stringify(options));
  form.append('file', file);
  if (attachments) form.append('attachments', attachments);
  return request<TmJob>(`${projectPath(key)}/imports`, { method: 'POST', body: form });
}

/** Mapping targets of a generic CSV column (`field:<key>` for case fields). */
export const IMPORT_TARGETS: { value: string; label: string }[] = [
  { value: 'ignore', label: 'Ignore' },
  { value: 'name', label: 'Name' },
  { value: 'folder', label: 'Folder path (A / B or A > B)' },
  { value: 'description', label: 'Description' },
  { value: 'preconditions', label: 'Preconditions' },
  { value: 'steps', label: 'Step action' },
  { value: 'expected', label: 'Step expected result' },
  { value: 'data', label: 'Step test data' },
  { value: 'tags', label: 'Tags' },
  { value: 'status', label: 'Status' },
  { value: 'key', label: 'External id (for repeated imports)' },
  { value: 'links', label: 'Links' },
  { value: 'attachments', label: 'Attachment file names' },
];

/** Formats read from CSV (the mapping and the steps layout apply). */
export function isCsvFormat(f: ImportFormat): boolean {
  return f !== 'qase-json' && f !== 'fastproof-ndjson';
}

export function createTestOpsImport(key: string, body: TestOpsImport): Promise<TmJob> {
  return request<TmJob>(`${projectPath(key)}/imports`, { method: 'POST', json: body });
}

export function useImportJob(key: string, id: string | null) {
  return useQuery({
    queryKey: tmk.job(key, id ?? ''),
    queryFn: () => request<TmJob>(`${projectPath(key)}/imports/${enc(id ?? '')}`),
    enabled: !!id,
    refetchInterval: (q) => {
      const s = q.state.data?.status;
      return s === 'queued' || s === 'dry_run' || s === 'running' ? 1000 : false;
    },
  });
}

export function applyImport(key: string, id: string): Promise<TmJob> {
  return request<TmJob>(`${projectPath(key)}/imports/${enc(id)}/apply`, { method: 'POST' });
}

export function cancelImport(key: string, id: string): Promise<unknown> {
  return request<unknown>(`${projectPath(key)}/imports/${enc(id)}`, { method: 'DELETE' });
}

/** Downloads a file with the API credentials (a plain link would lack the bearer token) and saves it. */
export async function downloadAuthed(path: string, filename: string): Promise<void> {
  const res = await apiFetch(path);
  if (!res.ok) throw await apiErrorFrom(res);
  const url = URL.createObjectURL(await res.blob());
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

export function exportCasesUrl(key: string, format: 'csv' | 'ndjson', q: CaseQuery): string {
  return casesUrl(key, q, { format }, '/export');
}

export function createCaseExport(key: string, filter: CaseFilter): Promise<TmJob> {
  return request<TmJob>(`${projectPath(key)}/cases/exports`, { method: 'POST', json: { format: 'zip', filter, attachments: true } });
}

export function useTmJob(key: string, id: string | null) {
  return useQuery({
    queryKey: [...tmk.job(key, id ?? ''), 'any'],
    queryFn: () => request<TmJob>(`${projectPath(key)}/tm-jobs/${enc(id ?? '')}`),
    enabled: !!id,
    refetchInterval: (q) => (!q.state.data || ['queued', 'dry_run', 'running'].includes(q.state.data.status) ? 1000 : false),
  });
}

// Live events

export interface CaseUpdatedEvent {
  number: number;
  revision: number;
  source: string;
}
export interface RunItemUpdatedEvent {
  launchId: string;
  id: string;
  status: RunStatus;
  revision: number;
}
export interface ManualRunUpdatedEvent {
  launchId: string;
  manual: LaunchManual;
}
export interface TmJobUpdatedEvent {
  id: string;
  kind: TmJob['kind'];
  status: TmJob['status'];
}

export interface TmEventHandlers {
  caseUpdated?: (e: CaseUpdatedEvent) => void;
  caseArchived?: (e: { number: number }) => void;
  runItemUpdated?: (e: RunItemUpdatedEvent) => void;
  manualRunUpdated?: (e: ManualRunUpdatedEvent) => void;
  tmJobUpdated?: (e: TmJobUpdatedEvent) => void;
}

function parseEvent(data: string): unknown {
  try {
    return JSON.parse(data) as unknown;
  } catch {
    return null;
  }
}

/** Subscribes to the project's TM events; handlers may change between renders. */
export function useTmEvents(key: string | undefined, handlers: TmEventHandlers): boolean {
  const [live, setLive] = useState(false);
  const ref = useRef(handlers);
  useEffect(() => {
    ref.current = handlers;
  });
  useEffect(() => {
    if (!key || !eventStreamSupported()) return;
    const es = openEventStream(`${projectPath(key)}/events`, {
      onOpen: () => setLive(true),
      onError: () => setLive(false),
      events: {
        'case.updated': (d) => {
          const e = parseEvent(d) as CaseUpdatedEvent | null;
          if (e) ref.current.caseUpdated?.(e);
        },
        'case.archived': (d) => {
          const e = parseEvent(d) as { number: number } | null;
          if (e) ref.current.caseArchived?.(e);
        },
        'runItem.updated': (d) => {
          const e = parseEvent(d) as RunItemUpdatedEvent | null;
          if (e) ref.current.runItemUpdated?.(e);
        },
        'manualRun.updated': (d) => {
          const e = parseEvent(d) as ManualRunUpdatedEvent | null;
          if (e) ref.current.manualRunUpdated?.(e);
        },
        'tmJob.updated': (d) => {
          const e = parseEvent(d) as TmJobUpdatedEvent | null;
          if (e) ref.current.tmJobUpdated?.(e);
        },
      },
    });
    return () => es.close();
  }, [key, ref]);
  return live;
}
