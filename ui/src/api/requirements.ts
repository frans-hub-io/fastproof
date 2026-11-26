import { keepPreviousData, useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useRef } from 'react';
import { ApiError, errorMessage, projectPath, request, requestRaw, withQuery } from './client';
import { eventStreamSupported, openEventStream } from './sse';
import { getVersioned, writeVersioned, type TmJob, type Versioned } from './tm';
import type { components } from './schema.gen';

type S = components['schemas'];

export type Requirement = S['Requirement'];
export type RequirementSummary = S['RequirementSummary'];
export type RequirementList = S['RequirementList'];
export type RequirementWrite = S['RequirementWrite'];
export type RequirementPatch = S['RequirementPatch'];
export type RequirementLinksChange = S['RequirementLinksChange'];
export type RequirementLink = S['RequirementLink'];
export type RequirementLinkList = S['RequirementLinkList'];
export type CaseRequirement = S['CaseRequirement'];
export type RequirementImport = S['RequirementImport'];
export type RequirementStatus = S['RequirementStatus'];
export type RequirementLinkOrigin = S['RequirementLinkOrigin'];
export type RequirementLinkStatus = S['RequirementLinkStatus'];
export type CoverageCounts = S['CoverageCounts'];
export type CoverageStatus = S['CoverageStatus'];
export type RequirementCoverage = S['RequirementCoverage'];
export type CoverageScopeView = S['CoverageScopeView'];
export type CoveragePage = S['CoveragePage'];
export type CoverageExportRequest = S['CoverageExportRequest'];
export type LastResult = S['LastResult'];

export const REQ_PAGE = 100;
export const REQUIREMENT_STATUSES: readonly RequirementStatus[] = ['draft', 'active', 'done', 'obsolete'];

export const rqk = {
  all: (key: string) => ['requirements', key] as const,
  list: (key: string, q: object) => ['requirements', key, 'list', q] as const,
  tree: (key: string, parent: number | null) => ['requirements', key, 'tree', parent] as const,
  one: (key: string, n: number) => ['requirements', key, 'one', n] as const,
  cases: (key: string, n: number, status: string) => ['requirements', key, 'cases', n, status] as const,
  ofCase: (key: string, caseNumber: number) => ['requirements', key, 'of-case', caseNumber] as const,
  coverage: (key: string, scope: object, parent: number | null) => ['requirements', key, 'coverage', scope, parent] as const,
};

/** The key a person types and sees: `REQ-12`. */
export function requirementKey(n: number): string {
  return `REQ-${String(n)}`;
}

/** `REQ-12`, `req12`, `#12` or `12` give 12. */
export function parseRequirementNumber(text: string): number | null {
  const m = /^\s*(?:req-?|#)?(\d{1,9})\s*$/i.exec(text);
  return m ? Number(m[1]) : null;
}

/** Splits a list of case numbers (`TC-1, 2 3`) and reports the entries that are not numbers. */
export function parseCaseNumbers(text: string): { numbers: number[]; invalid: string[] } {
  const numbers: number[] = [];
  const invalid: string[] = [];
  for (const part of text.split(/[\s,;]+/).filter(Boolean)) {
    const m = /^(?:tc-?|#)?(\d{1,9})$/i.exec(part);
    if (m) {
      const n = Number(m[1]);
      if (!numbers.includes(n)) numbers.push(n);
    } else invalid.push(part);
  }
  return { numbers, invalid };
}

const REQUIREMENT_ERRORS: Record<string, string> = {
  requirement_cycle: 'A requirement cannot move under itself or one of its own children.',
  issue_key_taken: 'Another requirement already uses this issue key.',
  invalid_query: 'The query is not valid.',
};

export function requirementError(e: unknown): string {
  if (e instanceof ApiError) {
    const known = REQUIREMENT_ERRORS[e.code];
    if (known) return known;
    if (e.code === 'limit_reached') {
      const d = (e.details ?? {}) as { limit?: unknown; max?: unknown };
      return `${e.message}${typeof d.max === 'number' ? ` (at most ${String(d.max)})` : ''}`;
    }
  }
  return errorMessage(e);
}

/** The position and message of an FPQL error, when the server sent them. */
export function queryErrorOf(e: unknown): { message: string; position?: number; length?: number } | null {
  if (!(e instanceof ApiError) || e.code !== 'invalid_query') return null;
  const d = (e.details ?? {}) as { position?: number; length?: number; message?: string };
  return { message: d.message ?? e.message, position: d.position, length: d.length };
}

export interface RequirementQuery {
  q?: string;
  status?: string;
  type?: string;
  query?: string;
  archived?: boolean;
  parent?: number;
}

export function useRequirements(key: string, q: RequirementQuery, enabled = true) {
  return useInfiniteQuery({
    queryKey: rqk.list(key, q),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      request<RequirementList>(
        withQuery(`${projectPath(key)}/requirements`, { q: q.q, status: q.status, type: q.type, query: q.query, parent: q.parent, archived: q.archived ? 'true' : undefined, before: pageParam, limit: REQ_PAGE }),
      ),
    getNextPageParam: (last) => last.next ?? undefined,
    placeholderData: keepPreviousData,
    retry: (count, e) => !(e instanceof ApiError && e.status < 500) && count < 2,
    enabled,
  });
}

/** Direct children of a requirement (the top level for `null`), not archived. */
export function useRequirementTree(key: string, parent: number | null, enabled = true) {
  return useInfiniteQuery({
    queryKey: rqk.tree(key, parent),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => request<RequirementList>(withQuery(`${projectPath(key)}/requirements/tree`, { parent: parent ?? undefined, before: pageParam, limit: REQ_PAGE })),
    getNextPageParam: (last) => last.next ?? undefined,
    enabled,
  });
}

export function useRequirement(key: string, n: number) {
  return useQuery({
    queryKey: rqk.one(key, n),
    queryFn: () => getVersioned<Requirement>(`${projectPath(key)}/requirements/${String(n)}`),
    enabled: Number.isInteger(n) && n > 0,
    staleTime: 0,
  });
}

export function useCreateRequirement(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: RequirementWrite) => request<Requirement>(`${projectPath(key)}/requirements`, { method: 'POST', json: body }),
    onSuccess: () => void qc.invalidateQueries({ queryKey: rqk.all(key) }),
  });
}

export function usePatchRequirement(key: string, n: number) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ body, etag }: { body: RequirementPatch; etag: string }) => writeVersioned<Requirement>(`${projectPath(key)}/requirements/${String(n)}`, 'PATCH', etag, body),
    onSuccess: (v: Versioned<Requirement>) => {
      void qc.invalidateQueries({ queryKey: rqk.all(key) });
      qc.setQueryData(rqk.one(key, n), v);
    },
  });
}

export function useArchiveRequirement(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (n: number) => requestRaw(`${projectPath(key)}/requirements/${String(n)}`, { method: 'DELETE' }),
    onSuccess: () => void qc.invalidateQueries({ queryKey: rqk.all(key) }),
  });
}

export function useRestoreRequirement(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (n: number) => request<Requirement>(`${projectPath(key)}/requirements/${String(n)}/restore`, { method: 'POST' }),
    onSuccess: () => void qc.invalidateQueries({ queryKey: rqk.all(key) }),
  });
}

export function useChangeLinks(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ requirement, change }: { requirement: number; change: RequirementLinksChange }) => request<Requirement>(`${projectPath(key)}/requirements/${String(requirement)}/links`, { method: 'POST', json: change }),
    onSuccess: () => void qc.invalidateQueries({ queryKey: rqk.all(key) }),
  });
}

export function useRequirementCases(key: string, n: number, linkStatus?: RequirementLinkStatus, enabled = true) {
  return useInfiniteQuery({
    queryKey: rqk.cases(key, n, linkStatus ?? 'all'),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => request<RequirementLinkList>(withQuery(`${projectPath(key)}/requirements/${String(n)}/cases`, { linkStatus, before: pageParam, limit: REQ_PAGE })),
    getNextPageParam: (last) => last.next ?? undefined,
    enabled,
  });
}

export function useCaseRequirements(key: string, caseNumber: number) {
  return useQuery({
    queryKey: rqk.ofCase(key, caseNumber),
    queryFn: () => request<CaseRequirement[]>(`${projectPath(key)}/cases/${String(caseNumber)}/requirements`),
  });
}

export function startRequirementImport(key: string, body: RequirementImport): Promise<TmJob> {
  return request<TmJob>(`${projectPath(key)}/requirements/import`, { method: 'POST', json: body });
}

export function applyRequirementImport(key: string, jobId: string): Promise<TmJob> {
  return request<TmJob>(`${projectPath(key)}/requirements/import/${encodeURIComponent(jobId)}/apply`, { method: 'POST' });
}

/** What a coverage page counts; kept in the URL of the page. */
export interface CoverageScope {
  kind: CoverageScopeView['kind'];
  planId?: string;
  launchId?: string;
  launchIds?: string;
  days?: number;
  branch?: string;
  environment?: string;
}

export const DEFAULT_SCOPE: CoverageScope = { kind: 'default' };

export function scopeFromParams(p: URLSearchParams): CoverageScope {
  const kind = p.get('scope');
  const text = (k: string) => p.get(k)?.trim() || undefined;
  const days = Number(p.get('days'));
  if (kind === 'plan') return { kind, planId: text('plan'), launchId: text('launch') };
  if (kind === 'launches') return { kind, launchIds: text('launches') };
  if (kind === 'window') return { kind, days: Number.isInteger(days) && days >= 1 && days <= 365 ? days : 30, branch: text('branch'), environment: text('env') };
  return DEFAULT_SCOPE;
}

export function scopeToParams(s: CoverageScope): Record<string, string> {
  const out: Record<string, string> = {};
  if (s.kind === 'default') return out;
  out.scope = s.kind;
  if (s.kind === 'plan') {
    if (s.planId) out.plan = s.planId;
    if (s.launchId) out.launch = s.launchId;
  } else if (s.kind === 'launches') {
    if (s.launchIds) out.launches = s.launchIds;
  } else {
    out.days = String(s.days ?? 30);
    if (s.branch) out.branch = s.branch;
    if (s.environment) out.env = s.environment;
  }
  return out;
}

export function launchIdList(text: string | undefined): string[] {
  return (text ?? '').split(/[\s,;]+/).filter(Boolean);
}

/** Whether the scope has what it needs to be asked for. */
export function scopeReady(s: CoverageScope): boolean {
  if (s.kind === 'plan') return !!s.planId;
  if (s.kind === 'launches') return launchIdList(s.launchIds).length > 0;
  return true;
}

export function scopeQuery(s: CoverageScope): Record<string, string | number | undefined> {
  if (s.kind === 'plan') return { planId: s.planId, launchId: s.launchId };
  if (s.kind === 'launches') return { launchIds: launchIdList(s.launchIds).join(',') };
  if (s.kind === 'window') return { days: s.days, branch: s.branch, environment: s.environment };
  return {};
}

export function useCoverage(key: string, scope: CoverageScope, parent: number | null, enabled = true) {
  return useInfiniteQuery({
    queryKey: rqk.coverage(key, scope, parent),
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => request<CoveragePage>(withQuery(`${projectPath(key)}/coverage`, { ...scopeQuery(scope), parent: parent ?? undefined, before: pageParam, limit: REQ_PAGE })),
    getNextPageParam: (last) => last.next ?? undefined,
    enabled: enabled && scopeReady(scope),
    retry: (count, e) => !(e instanceof ApiError && e.status < 500) && count < 2,
  });
}

export function startCoverageExport(key: string, format: 'csv' | 'xlsx', scope: CoverageScope): Promise<TmJob> {
  const body: CoverageExportRequest = { format };
  if (scope.kind === 'plan') body.planId = scope.planId;
  if (scope.kind === 'launches') body.launchIds = launchIdList(scope.launchIds);
  if (scope.kind === 'window') {
    body.days = scope.days;
    body.branch = scope.branch;
    body.environment = scope.environment;
  }
  return request<TmJob>(`${projectPath(key)}/coverage/exports`, { method: 'POST', json: body });
}

/** Refreshes requirement data when a requirement or a background job changes elsewhere. */
export function useRequirementEvents(key: string | undefined): void {
  const qc = useQueryClient();
  const ref = useRef(qc);
  useEffect(() => {
    ref.current = qc;
  });
  useEffect(() => {
    if (!key || !eventStreamSupported()) return;
    const es = openEventStream(`${projectPath(key)}/events`, {
      events: {
        'requirement.updated': () => void ref.current.invalidateQueries({ queryKey: rqk.all(key) }),
        'tmJob.updated': () => void ref.current.invalidateQueries({ queryKey: ['tm', key, 'job'] }),
      },
    });
    return () => es.close();
  }, [key]);
}

/** The coverage of a requirement from its counts: failing wins, then all passed, then not run, else partly passed. */
export function coverageStatusOf(counts: CoverageCounts): CoverageStatus {
  if (counts.total === 0) return 'uncovered';
  if (counts.failed > 0) return 'failed';
  if (counts.passed === counts.total) return 'passed';
  if (counts.passed === 0) return 'notRun';
  return 'partial';
}

/** Coverage of one requirement from the last results of its active links (the default scope). */
export function coverageOfLinks(links: readonly Pick<RequirementLink, 'linkStatus' | 'lastResult'>[]): { status: CoverageStatus; counts: CoverageCounts } {
  const counts: CoverageCounts = { total: 0, passed: 0, failed: 0, notRun: 0, skipped: 0 };
  for (const l of links) {
    if (l.linkStatus !== 'active') continue;
    counts.total++;
    const s = l.lastResult?.status;
    if (!s) counts.notRun++;
    else if (s === 'passed') counts.passed++;
    else if (s === 'skipped') counts.skipped++;
    else counts.failed++;
  }
  const status = coverageStatusOf(counts);
  return { status, counts };
}
